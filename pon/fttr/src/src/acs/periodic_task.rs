use super::{json, now, Server, State, Task, TaskState, Value};
use crate::provisioning::model::ConnectionRequest;

#[derive(Clone, Copy)]
pub(super) enum Root {
    Igd,
    Device,
}

impl Root {
    pub(super) fn path(self, leaf: &str) -> String {
        let root = match self {
            Self::Igd => "InternetGatewayDevice",
            Self::Device => "Device",
        };
        format!("{root}.ManagementServer.{leaf}")
    }
}

#[derive(Clone, Copy)]
pub(super) struct Interval(u32);

impl Interval {
    pub(super) fn new(seconds: u32) -> Option<Self> {
        (1..=43200).contains(&seconds).then_some(Self(seconds))
    }
}

#[derive(Clone)]
pub(super) struct Authorization {
    config: ConnectionRequest,
    device: String,
    root: Root,
    interval: Interval,
    proof: super::connection_request_proof::Proof,
}

impl Authorization {
    fn args(&self) -> Value {
        json!({"parameters":[{"name":self.root.path("PeriodicInformInterval"),
            "value":self.interval.0.to_string(),"kind":"unsignedInt"}]})
    }

    pub(super) fn permits(&self, task: &Task, state: &State) -> bool {
        let mut args = task.args.clone();
        if let Some(object) = args.as_object_mut() {
            object.remove("_omci_auth_epoch");
        }
        self.config.enabled
            && self.config.credentials_available()
            && self
                .config
                .authorization
                .load(std::sync::atomic::Ordering::Acquire)
            && task.device == self.device
            && task.method == "SetParameterValues"
            && args == self.args()
            && super::connection_request_proof::ready(state, &self.device, &self.config)
            && state
                .connection_request_proofs
                .get(&self.device)
                .is_some_and(|proof| self.proof.same_attempt(proof))
    }
}

pub(super) enum Request {
    Read(Root),
    Capability(Root),
    Set {
        root: Root,
        interval: Interval,
        config: ConnectionRequest,
    },
}

pub(super) fn enqueue(server: &Server, state: &mut State, job: (&str, Request)) -> Option<u64> {
    if state
        .tasks
        .values()
        .filter(|task| task.state.is_active())
        .count()
        >= 128
    {
        return None;
    }
    let id = state.next_id.checked_add(1)?;
    let (device, request) = job;
    let (method, args, grant) = match request {
        Request::Read(root) => (
            "GetParameterValues",
            json!({"names":[root.path("PeriodicInformEnable"),root.path("PeriodicInformInterval")]}),
            None,
        ),
        Request::Capability(root) => (
            "GetParameterNames",
            json!({"path":root.path("PeriodicInformInterval"),"next_level":false}),
            None,
        ),
        Request::Set {
            root,
            interval,
            config,
        } => {
            super::policy::protect_unresolved(state, device, "SetParameterValues").ok()?;
            let proof = state.connection_request_proofs.get(device)?.clone();
            let grant = Authorization {
                config,
                device: device.into(),
                root,
                interval,
                proof,
            };
            ("SetParameterValues", grant.args(), Some(grant))
        }
    };
    let mut args = args;
    if method == "SetParameterValues" {
        args["_omci_auth_epoch"] = serde_json::json!(crate::omci_auth::cwmp_epoch(device)?);
    }
    let task = Task {
        id,
        device: device.into(),
        method: method.into(),
        args,
        state: TaskState::Queued,
        created: now(),
        sent: None,
        result: None,
    };
    if grant
        .as_ref()
        .is_some_and(|authorization| !authorization.permits(&task, state))
    {
        return None;
    }
    if server.enqueue(state, task)["error"].is_string() {
        return None;
    }
    if let Some(grant) = grant {
        state.interval_authorizations.insert(id, grant);
    }
    Some(id)
}
