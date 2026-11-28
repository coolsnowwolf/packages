use super::{connection_request_http::Target, Device, State, TaskState};
use crate::provisioning::model::ConnectionRequest;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(super) struct Proof {
    pub(super) config: ConnectionRequest,
    target: Target,
    endpoint: String,
    started: Instant,
    authenticated: bool,
    session: Option<String>,
    dispatched: Option<u64>,
    completed: bool,
}

pub(super) fn target(device: &Device, config: &ConnectionRequest) -> Option<Target> {
    let url = device.inform["parameters"].as_array()?.iter().find(|p| {
        matches!(
            p["name"].as_str(),
            Some(
                "InternetGatewayDevice.ManagementServer.ConnectionRequestURL"
                    | "Device.ManagementServer.ConnectionRequestURL"
            )
        )
    })?["value"]
        .as_str()?;
    Target::parse(url, &device.peer)
        .ok()
        .map(|t| t.effective(config))
}

impl Proof {
    pub(super) fn config(&self) -> ConnectionRequest {
        self.config.clone()
    }
    pub(super) fn new(device: &Device, config: &ConnectionRequest) -> Option<Self> {
        Some(Self {
            config: config.clone(),
            target: target(device, config)?,
            endpoint: device.endpoint_id.clone(),
            started: Instant::now(),
            authenticated: false,
            session: None,
            dispatched: None,
            completed: false,
        })
    }

    pub(super) fn current(&self, device: &Device, config: &ConnectionRequest) -> bool {
        self.config.same_policy(config)
            && config.enabled
            && config.credentials_available()
            && config
                .authorization
                .load(std::sync::atomic::Ordering::Acquire)
            && self.endpoint == device.endpoint_id
            && target(device, config).as_ref() == Some(&self.target)
    }

    pub(super) fn ready(&self, device: &Device, config: &ConnectionRequest) -> bool {
        self.authenticated
            && self.completed
            && self.started.elapsed() <= Duration::from_secs(30)
            && self.current(device, config)
    }

    pub(super) fn same_attempt(&self, other: &Self) -> bool {
        self.started == other.started
    }

    pub(super) fn authenticated(&mut self, accepted: bool) {
        self.authenticated = accepted;
    }

    pub(super) fn inform(&mut self, device: &Device, session: &str) {
        if !self.completed
            && self.started.elapsed() <= Duration::from_secs(30)
            && self.current(device, &self.config)
            && device.inform["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|e| e == "6 CONNECTION REQUEST"))
        {
            self.session = Some(session.into());
            self.dispatched = None;
        }
    }
}

pub(super) fn dispatched(state: &mut State, device: &str, session: &str, id: u64) {
    let Some(proof) = state.connection_request_proofs.get_mut(device) else {
        return;
    };
    if proof.session.as_deref() == Some(session)
        && proof.started.elapsed() <= Duration::from_secs(30)
        && state
            .tasks
            .get(&id)
            .is_some_and(|task| task.device == device && task.method == "GetParameterValues")
    {
        proof.dispatched = Some(id);
    }
}

pub(super) fn completed(state: &mut State, device: &str, session: &str) {
    let Some(proof) = state.connection_request_proofs.get_mut(device) else {
        return;
    };
    if proof.session.as_deref() == Some(session)
        && proof.started.elapsed() <= Duration::from_secs(30)
        && proof
            .dispatched
            .and_then(|id| state.tasks.get(&id))
            .is_some_and(|task| {
                task.device == device
                    && task.method == "GetParameterValues"
                    && task.state == TaskState::Completed
            })
    {
        proof.completed = true;
    }
}

pub(super) fn ready(state: &State, device: &str, config: &ConnectionRequest) -> bool {
    state
        .connection_request_proofs
        .get(device)
        .zip(state.devices.get(device))
        .is_some_and(|(proof, device)| proof.ready(device, config))
}
