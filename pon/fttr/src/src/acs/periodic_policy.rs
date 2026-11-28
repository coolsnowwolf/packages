use super::{
    now,
    periodic_task::{self, Interval, Request, Root},
    Server, TaskState,
};
use crate::provisioning::model::ConnectionRequest;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Reading,
    AwaitingProof,
    Capability,
    Setting,
    Readback,
    Verified,
    ManualRequired,
    DefaultCredentialsUnavailable,
    MissingCredentials,
    Unsupported,
    Unreachable,
    Unknown,
}

#[derive(Clone, Serialize)]
pub(super) struct Policy {
    state: Phase,
    connection_request_enabled: bool,
    expected_interval: Option<u32>,
    observed_enable: Option<bool>,
    observed_interval: Option<u32>,
    verified_at: Option<u64>,
    reason: &'static str,
    #[serde(skip)]
    config: ConnectionRequest,
    #[serde(skip)]
    root: Root,
    #[serde(skip)]
    pending: Option<u64>,
    #[serde(skip)]
    last_inform: u64,
}

impl Policy {
    fn new(config: &ConnectionRequest, root: Root, last_inform: u64) -> Self {
        Self {
            state: Phase::Reading,
            connection_request_enabled: config.enabled,
            expected_interval: config.enabled.then_some(43200),
            observed_enable: None,
            observed_interval: None,
            verified_at: None,
            reason: "current GPV evidence required",
            config: config.clone(),
            root,
            pending: None,
            last_inform,
        }
    }

    fn stop(&mut self, phase: Phase, reason: &'static str) {
        self.state = phase;
        self.reason = reason;
        self.verified_at = None;
    }

    fn verify(&mut self) {
        self.state = Phase::Verified;
        self.reason = "independent GPV confirms enabled periodic Inform and policy interval";
        self.verified_at = Some(now());
    }

    fn read(&mut self, data: &crate::cwmp::response::Data) -> bool {
        let parameters = data.rows();
        let value = |leaf| {
            parameters
                .into_iter()
                .flatten()
                .find(|p| p["name"] == self.root.path(leaf))
                .and_then(|p| p["value"].as_str())
        };
        let enabled = match value("PeriodicInformEnable") {
            Some("true" | "1") => Some(true),
            Some("false" | "0") => Some(false),
            _ => None,
        };
        let interval = value("PeriodicInformInterval").and_then(|v| v.parse::<u32>().ok());
        self.observed_enable = enabled;
        self.observed_interval = interval;
        enabled.is_some() && interval.is_some_and(|value| value > 0)
    }

    fn advance(&mut self, result: &crate::cwmp::response::Data) {
        match self.state {
            Phase::Reading | Phase::Readback => {
                let readback = self.state == Phase::Readback;
                if !self.read(result) {
                    self.stop(
                        Phase::Unsupported,
                        "GPV did not provide valid periodic Inform parameters",
                    );
                } else if !self.config.enabled {
                    if self.observed_enable == Some(true)
                        && self.observed_interval.is_some_and(|v| v <= 180)
                    {
                        self.verify();
                    } else {
                        self.stop(Phase::ManualRequired,"CR off: manually enable PeriodicInformEnable and set interval 1..180; no automatic writes");
                    }
                } else if self.config.defaults {
                    self.stop(
                        Phase::DefaultCredentialsUnavailable,
                        "no verified factory Connection Request credential profile is available",
                    );
                } else if !self.config.credentials_available() {
                    self.stop(
                        Phase::MissingCredentials,
                        "custom Connection Request credentials are missing or invalid",
                    );
                } else if self.observed_enable != Some(true) {
                    self.stop(Phase::ManualRequired,"manually enable PeriodicInformEnable; interval-only authorization cannot enable it");
                } else if readback {
                    if self.observed_interval == Some(43200) {
                        self.verify();
                    } else {
                        self.stop(
                            Phase::Unknown,
                            "SPV accepted but independent GPV did not confirm 43200 seconds",
                        );
                    }
                } else {
                    self.state = Phase::AwaitingProof;
                    self.reason =
                        "authenticated CR, ensuing event 6 session and completed GPV required";
                }
            }
            Phase::Capability => {
                let writable = result.rows().into_iter().flatten().any(|p| {
                    p["name"] == self.root.path("PeriodicInformInterval")
                        && matches!(p["writable"].as_str(), Some("true" | "1"))
                });
                if !writable {
                    self.stop(
                        Phase::Unsupported,
                        "GPN does not prove the exact interval parameter is supported and writable",
                    );
                } else if self.observed_interval == Some(43200) {
                    self.verify();
                } else {
                    self.state = Phase::Setting;
                }
            }
            Phase::Setting => {
                self.state = Phase::Readback;
                self.reason = "SPV acceptance is not verification; independent GPV required";
            }
            Phase::AwaitingProof
            | Phase::Verified
            | Phase::ManualRequired
            | Phase::DefaultCredentialsUnavailable
            | Phase::MissingCredentials
            | Phase::Unsupported
            | Phase::Unreachable
            | Phase::Unknown => {}
        }
    }

    fn request(&self) -> Option<Request> {
        match self.state {
            Phase::Reading | Phase::Readback => Some(Request::Read(self.root)),
            Phase::Capability => Some(Request::Capability(self.root)),
            Phase::Setting => Interval::new(43200).map(|interval| Request::Set {
                root: self.root,
                interval,
                config: self.config.clone(),
            }),
            Phase::AwaitingProof
            | Phase::Verified
            | Phase::ManualRequired
            | Phase::DefaultCredentialsUnavailable
            | Phase::MissingCredentials
            | Phase::Unsupported
            | Phase::Unreachable
            | Phase::Unknown => None,
        }
    }
}

pub(super) fn poll(server: &Server, configs: &BTreeMap<String, ConnectionRequest>) {
    let Ok(mut state) = server.state.lock() else {
        return;
    };
    if !server.enabled
        || server.stopped.load(std::sync::atomic::Ordering::Acquire)
        || state.load_failure.is_some()
    {
        return;
    }
    state.expire();
    let tasks = &state.tasks;
    let retained = tasks
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    state
        .interval_authorizations
        .retain(|id, _| retained.contains(id));
    for (id, config) in configs {
        let Some(device) = state.devices.get(id) else {
            continue;
        };
        let at = device.last_inform;
        let root = if device.inform["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| {
                p["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Device."))
            }) {
            Root::Device
        } else {
            Root::Igd
        };
        if state
            .connection_request_proofs
            .get(id)
            .is_some_and(|proof| !proof.current(device, config))
        {
            state.connection_request_proofs.remove(id);
        }
        let mut policy = state
            .periodic_inform
            .remove(id)
            .unwrap_or_else(|| Policy::new(config, root, at));
        if !policy.config.same_policy(config) {
            if let Some(task) = policy.pending.and_then(|id| state.tasks.get_mut(&id)) {
                if task.state == TaskState::Queued {
                    task.state = TaskState::Expired;
                }
            }
            policy = Policy::new(config, root, at);
        } else if policy.pending.is_none() && policy.last_inform != at {
            policy = Policy::new(config, root, at);
        }
        policy.last_inform = at;
        if let Some(task_id) = policy.pending {
            match state.tasks.get(&task_id) {
                Some(task) => match task.state {
                    TaskState::Queued | TaskState::Sent => {}
                    TaskState::Completed | TaskState::AcceptedPendingApply => {
                        let result = task.result.as_ref().map(|r| r.get());
                        policy.pending = None;
                        match result {
                            Some(result) => policy.advance(&result),
                            None => policy.stop(Phase::Unknown, "task result unavailable"),
                        }
                    }
                    TaskState::Expired => {
                        policy.pending = None;
                        policy.stop(Phase::Unreachable, "no CPE session; policy not verified");
                    }
                    TaskState::Fault => {
                        policy.pending = None;
                        policy.stop(
                            Phase::Unsupported,
                            "periodic Inform RPC fault; policy not verified",
                        );
                    }
                    TaskState::Unknown => {
                        policy.pending = None;
                        policy.stop(
                            Phase::Unknown,
                            "RPC execution unknown; no automatic write replay",
                        );
                    }
                },
                None => {
                    policy.pending = None;
                    policy.stop(
                        Phase::Unknown,
                        "task history unavailable; new Inform required",
                    );
                }
            }
        }
        if policy.state == Phase::AwaitingProof
            && super::connection_request_proof::ready(&state, id, config)
        {
            policy.state = Phase::Capability;
        }
        if policy.pending.is_none() {
            if let Some(request) = policy.request() {
                policy.pending = periodic_task::enqueue(server, &mut state, (id, request));
            }
        }
        state.periodic_inform.insert(id.clone(), policy);
    }
    state
        .periodic_inform
        .retain(|id, _| configs.contains_key(id));
}
