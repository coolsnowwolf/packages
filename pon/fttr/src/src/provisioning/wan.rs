//! Discover every connection, protect fallback services, then reconcile in order.
use super::model::{ItemKind, ItemOutcome, TaskOutcome, WanOwnershipState, WanPhase};
use super::policy::protected;
use super::{acs, config::Device};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const ROOT: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.";
fn registry_path(id: &str) -> String {
    super::runtime_store::Record::Wan.path(id)
}
pub fn load(id: &str) -> Result<Option<BTreeMap<String, Owned>>, String> {
    let path = registry_path(id);
    let text = match super::storage::read_to_string(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read WAN ownership: {e}")),
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid WAN ownership: {e}"))?;
    if v["cwmp_id"] != id {
        return Err("WAN ownership identity mismatch".into());
    }
    serde_json::from_value(v["owned"].clone())
        .map(Some)
        .map_err(|e| format!("invalid WAN ownership records: {e}"))
}
#[cfg(not(test))]
pub fn reset(id: &str) -> Result<(), String> {
    super::storage::remove_registry(&registry_path(id))
}
#[cfg(feature = "acs")]
pub fn parent_protected(id: &str, parent: u32) -> bool {
    super::storage::read_to_string(registry_path(id))
        .ok()
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .is_none_or(|v| {
            v["protected_parents"]
                .as_array()
                .is_some_and(|a| a.iter().any(|p| p.as_u64() == Some(parent as u64)))
        })
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Owned {
    pub parent: u32,
    pub connection: u32,
    pub state: WanOwnershipState,
    pub service_list: String,
    #[serde(default = "ip_kind")]
    pub kind: String,
    #[serde(default)]
    pub baseline: Option<BTreeSet<u32>>,
}
fn ip_kind() -> String {
    "WANIPConnection".into()
}
#[derive(Clone)]
pub struct Reconciler {
    submission: Option<crate::cwmp::transport::Pending<WanPhase>>,
    pub evidence_paths: BTreeMap<String, Vec<String>>,
    pub protected_roots: BTreeSet<String>,
    pub owned: BTreeMap<String, Owned>,
    pub results: BTreeMap<String, Value>,
    actions: VecDeque<(String, bool)>,
    pub stage: WanPhase,
    task: Option<u64>,
    current: Option<(String, bool)>,
    initialized: bool,
    discovery: VecDeque<(u32, u32, String)>,
    discovered: Vec<Owned>,
    prune: bool,
    replacing: BTreeSet<String>,
    parents: BTreeSet<u32>,
    vlans: BTreeMap<u32, (u32, bool)>,
    priorities: BTreeMap<u32, u32>,
    final_started: bool,
    final_apply: bool,
}
impl Reconciler {
    pub(super) fn progress(&self) -> (WanPhase, Option<u64>, bool, usize, usize, bool) {
        (
            self.stage,
            self.task,
            self.initialized,
            self.discovery.len(),
            self.actions.len(),
            self.final_started,
        )
    }
    pub fn new(device: &Device, owned: BTreeMap<String, Owned>, prune: bool) -> Self {
        let _ = device;
        Self {
            evidence_paths: BTreeMap::new(),
            protected_roots: BTreeSet::new(),
            owned,
            results: BTreeMap::new(),
            actions: VecDeque::new(),
            stage: WanPhase::Waiting,
            task: None,
            submission: None,
            current: None,
            initialized: false,
            discovery: VecDeque::new(),
            discovered: Vec::new(),
            prune,
            replacing: BTreeSet::new(),
            parents: BTreeSet::new(),
            vlans: BTreeMap::new(),
            priorities: BTreeMap::new(),
            final_started: false,
            final_apply: false,
        }
    }
    pub fn begin_final_verification(&mut self, apply: bool) -> bool {
        if self.final_started || !self.prune {
            return false;
        }
        self.final_started = true;
        self.final_apply = apply;
        self.initialized = false;
        self.submission = None;
        self.stage = WanPhase::Waiting;
        self.parents.clear();
        self.vlans.clear();
        true
    }
    pub fn full_sync(&self) -> bool {
        self.prune
    }
    fn queue(&mut self, device: &Device, method: &str, args: Value, stage: WanPhase) {
        self.submission = Some(crate::cwmp::transport::Pending {
            method: method.into(),
            args,
            stage,
        });
        self.submit(device);
    }
    fn submit(&mut self, device: &Device) {
        if self.save(&device.cwmp_id).is_err() {
            self.fail(
                device,
                ItemOutcome::Blocked,
                "cannot persist WAN ownership before lifecycle RPC",
            );
            return;
        }
        let pending = self.submission.as_ref().unwrap();
        let v = acs(Command::queue(
            &device.cwmp_id,
            &pending.method,
            pending.args.clone(),
        ));
        if let Some(id) = v["id"].as_u64() {
            self.task = Some(id);
            self.stage = self.submission.take().unwrap().stage;
        } else if crate::cwmp::transport::backpressure(&v) {
            self.task = None;
        } else {
            self.submission = None;
            self.fail(
                device,
                ItemOutcome::Failed,
                "cannot queue WAN lifecycle RPC",
            );
        }
    }
    fn fail(&mut self, device: &Device, state: ItemOutcome, reason: &str) {
        let id = self
            .current
            .as_ref()
            .map(|a| a.0.as_str())
            .unwrap_or("inventory");
        self.results.insert(
            id.into(),
            json!({"state":state,"reason":reason,"task_id":self.task,"cwmp_id":device.cwmp_id}),
        );
        self.task = None;
        self.submission = None;
        self.current = None;
        self.stage = WanPhase::Next;
    }
    pub fn tick(
        &mut self,
        device: &mut Device,
        apply: bool,
        _management: bool,
        reachable: bool,
    ) -> bool {
        if !self.initialized
            && device
                .items
                .iter()
                .any(|item| item.wan_ref.is_some() && item.blocked.is_some())
        {
            self.results.entry("inventory".into()).or_insert_with(|| json!({"state":"blocked","reason":"WAN plan or binding failed preflight; lifecycle changes withheld"}));
            self.stage = WanPhase::Done;
            return true;
        }
        if !self.prune && !device.items.iter().any(|i| i.wan_ref.is_some()) {
            self.stage = WanPhase::Done;
            return true;
        }
        if self.stage == WanPhase::Done {
            return true;
        }
        if self.initialized
            && self.actions.is_empty()
            && self.current.is_none()
            && self.task.is_none()
            && self.stage == WanPhase::Next
        {
            if apply {
                self.order_mapping(device);
            }
            self.stage = WanPhase::Done;
            return true;
        }
        if !reachable {
            return false;
        }
        if self.submission.is_some() {
            self.submit(device);
            return false;
        }
        if self.task.is_none()
            && acs(Command::Busy {
                device: device.cwmp_id.clone(),
            })["busy"]
                == true
        {
            return false;
        }
        if !self.initialized {
            self.initialized = true;
            self.queue(
                device,
                "GetParameterNames",
                json!({"path":ROOT,"next_level":false}),
                WanPhase::Inventory,
            );
            return false;
        }
        if let Some(task) = self.task {
            let v = acs(Command::Task { id: task });
            let state = TaskOutcome::from_task(&v);
            if matches!(state, TaskOutcome::Queued | TaskOutcome::Sent) {
                return false;
            }
            if !matches!(
                state,
                TaskOutcome::Completed | TaskOutcome::AcceptedPendingApply
            ) {
                if matches!(state, TaskOutcome::Fault | TaskOutcome::Expired)
                    && self.stage == WanPhase::AddingParent
                {
                    if let Some((id, _)) = &self.current {
                        self.owned.remove(id);
                    }
                }
                self.fail(
                    device,
                    if state == TaskOutcome::Unknown {
                        ItemOutcome::Unknown
                    } else {
                        ItemOutcome::Failed
                    },
                    "WAN lifecycle RPC failed/unknown; no write replay",
                );
                if self.stage == WanPhase::Next
                    && self.current.is_none()
                    && !self.results.contains_key("inventory")
                {
                    return false;
                }
                self.actions.clear();
                self.stage = WanPhase::Done;
                return true;
            }
            self.task = None;
            if matches!(
                self.stage,
                WanPhase::Inventory
                    | WanPhase::Classifying
                    | WanPhase::ChildNames
                    | WanPhase::DeleteGuard
                    | WanPhase::VerifyDelete
                    | WanPhase::EmptyParent
                    | WanPhase::VerifyParentDelete
            ) && v.result().rows().is_none()
            {
                self.fail(
                    device,
                    ItemOutcome::Blocked,
                    "incomplete WAN RPC inventory/readback; no mutation allowed",
                );
                self.actions.clear();
                self.stage = WanPhase::Done;
                return true;
            }
            match self.stage {
                WanPhase::Inventory => {
                    let mut connections = BTreeSet::new();
                    for p in v.result().rows().into_iter().flatten() {
                        if let Some(n) = p["name"].as_str().and_then(|n| n.strip_prefix(ROOT)) {
                            let parts: Vec<_> = n.split('.').collect();
                            if let Ok(parent) = parts[0].parse::<u32>() {
                                if parent > 0 {
                                    self.parents.insert(parent);
                                }
                            }
                            if parts.len() >= 3
                                && matches!(parts[1], "WANIPConnection" | "WANPPPConnection")
                            {
                                if let (Ok(parent), Ok(child)) =
                                    (parts[0].parse::<u32>(), parts[2].parse::<u32>())
                                {
                                    if parent > 0 && child > 0 {
                                        connections.insert((parent, child, parts[1].to_string()));
                                    }
                                }
                            }
                        }
                    }
                    self.discovery = connections.into_iter().collect();
                    self.discovered.clear();
                    self.discover_next(device);
                    if self.stage != WanPhase::Next {
                        return self.stage == WanPhase::Done;
                    }
                }
                WanPhase::Classifying => {
                    let (parent, connection, kind) = self.discovery.pop_front().unwrap();
                    let parameters = v.result().rows();
                    let value = |suffix: &str| {
                        parameters
                            .and_then(|a| {
                                a.iter().find(|p| {
                                    p["name"].as_str().is_some_and(|n| n.ends_with(suffix))
                                })
                            })
                            .and_then(|p| p["value"].as_str())
                    };
                    let contract = device.contract().wan;
                    let service = value(contract.service.name);
                    let vlan = value(contract.vlan.name).and_then(|v| v.parse::<u32>().ok());
                    let enabled = contract.tagged(|name| value(name).map(str::to_owned));
                    let (Some(service), Some(vlan), Some(enabled)) = (service, vlan, enabled)
                    else {
                        self.fail(
                            device,
                            ItemOutcome::Blocked,
                            "incomplete WAN classification; no changes allowed",
                        );
                        self.actions.clear();
                        self.stage = WanPhase::Done;
                        return true;
                    };
                    self.vlans.insert(parent, (vlan, enabled));
                    if contract.priority.location == crate::cwmp::contract::Location::Parent {
                        let Some(priority) =
                            value(contract.priority.name).and_then(|v| v.parse::<u32>().ok())
                        else {
                            self.fail(
                                device,
                                ItemOutcome::Blocked,
                                "incomplete parent priority readback",
                            );
                            self.stage = WanPhase::Done;
                            return true;
                        };
                        self.priorities.insert(parent, priority);
                    }
                    self.discovered.push(Owned {
                        parent,
                        connection,
                        kind: kind.clone(),
                        state: if protected(service, enabled)
                            || crate::cwmp::observed::protected(
                                parameters.unwrap_or_default(),
                                &format!("{ROOT}{parent}.{kind}.{connection}."),
                            ) {
                            WanOwnershipState::Protected
                        } else {
                            WanOwnershipState::Ready
                        },
                        service_list: service.into(),
                        baseline: None,
                    });
                    self.discover_next(device);
                    return false;
                }
                WanPhase::AddingParent => {
                    let n = v.result()["instance_number"]
                        .as_str()
                        .and_then(|n| n.parse::<u32>().ok())
                        .filter(|n| *n > 0);
                    let Some(n) = n else {
                        self.fail(
                            device,
                            ItemOutcome::Unknown,
                            "AddObject did not return an instance",
                        );
                        return false;
                    };
                    self.parents.insert(n);
                    let id = self.current.as_ref().unwrap().0.clone();
                    let o = self.owned.get_mut(&id).unwrap();
                    o.parent = n;
                    o.state = WanOwnershipState::DiscoveringChild;
                    self.queue(
                        device,
                        "GetParameterNames",
                        json!({"path":format!("{ROOT}{n}."),"next_level":false}),
                        WanPhase::ChildNames,
                    );
                    return false;
                }
                WanPhase::ChildNames => {
                    let id = self.current.as_ref().unwrap().0.clone();
                    let o = &self.owned[&id];
                    let prefix = format!("{ROOT}{}.WANIPConnection.", o.parent);
                    let children: BTreeSet<u32> = v
                        .result()
                        .rows()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| {
                            p["name"]
                                .as_str()?
                                .strip_prefix(&prefix)?
                                .split('.')
                                .next()?
                                .parse::<u32>()
                                .ok()
                        })
                        .collect();
                    if children.len() > 1 {
                        self.fail(
                            device,
                            ItemOutcome::Blocked,
                            "new WAN unexpectedly has multiple IP connections",
                        );
                        return false;
                    }
                    if let Some(n) = children.first() {
                        let o = self.owned.get_mut(&id).unwrap();
                        o.connection = *n;
                        o.state = WanOwnershipState::Ready;
                        self.remap(device, &id);
                        self.current = None;
                        self.stage = WanPhase::Next;
                    } else {
                        self.owned.get_mut(&id).unwrap().state = WanOwnershipState::AddingChild;
                        self.queue(
                            device,
                            "AddObject",
                            json!({"object":prefix}),
                            WanPhase::AddingChild,
                        );
                        return false;
                    }
                }
                WanPhase::AddingChild => {
                    let n = v.result()["instance_number"]
                        .as_str()
                        .and_then(|n| n.parse::<u32>().ok())
                        .filter(|n| *n > 0);
                    let Some(n) = n else {
                        self.fail(
                            device,
                            ItemOutcome::Unknown,
                            "child AddObject did not return an instance",
                        );
                        return false;
                    };
                    let id = self.current.as_ref().unwrap().0.clone();
                    let o = self.owned.get_mut(&id).unwrap();
                    o.connection = n;
                    o.state = WanOwnershipState::Ready;
                    self.remap(device, &id);
                    self.current = None;
                    self.stage = WanPhase::Next;
                }
                WanPhase::DeleteGuard => {
                    let id = self.current.as_ref().unwrap().0.clone();
                    let o = &self.owned[&id];
                    if self
                        .protected_roots
                        .contains(&format!("{ROOT}{}.{}.{}.", o.parent, o.kind, o.connection))
                    {
                        self.fail(
                            device,
                            ItemOutcome::Blocked,
                            "cross-contract discovery protects this management/fallback WAN",
                        );
                        return false;
                    }
                    let parameters = v.result().rows();
                    let value = |suffix: &str| {
                        parameters
                            .and_then(|a| {
                                a.iter().find(|p| {
                                    p["name"].as_str().is_some_and(|n| n.ends_with(suffix))
                                })
                            })
                            .and_then(|p| p["value"].as_str())
                    };
                    let contract = device.contract().wan;
                    let observed = value(contract.service.name);
                    let Some(observed) = observed else {
                        self.fail(
                            device,
                            ItemOutcome::Blocked,
                            "missing service list before deleting owned WAN",
                        );
                        return false;
                    };
                    if crate::cwmp::observed::protected(
                        parameters.unwrap_or_default(),
                        &format!("{ROOT}{}.{}.{}.", o.parent, o.kind, o.connection),
                    ) || super::contains_service_token(observed, "TR069")
                        || (!observed.is_empty() && observed != o.service_list)
                    {
                        self.fail(device,ItemOutcome::Blocked,"owned WAN now carries protected or reassigned service; manual review required");
                        return false;
                    }
                    let enabled = contract.tagged(|name| value(name).map(str::to_owned));
                    if enabled.is_none_or(|enabled| protected(observed, enabled)) {
                        self.fail(
                            device,
                            ItemOutcome::Blocked,
                            "WAN became a protected fallback before deletion",
                        );
                        return false;
                    }
                    if !apply {
                        self.fail(
                            device,
                            ItemOutcome::Drift,
                            "owned WAN removed from desired configuration; apply will delete",
                        );
                        return false;
                    }
                    let object = format!("{ROOT}{}.{}.{}.", o.parent, o.kind, o.connection);
                    self.owned.get_mut(&id).unwrap().state = WanOwnershipState::Deleting;
                    self.queue(
                        device,
                        "DeleteObject",
                        json!({"object":object}),
                        WanPhase::Deleting,
                    );
                    return false;
                }
                WanPhase::Deleting => {
                    let id = &self.current.as_ref().unwrap().0;
                    let o = &self.owned[id];
                    let path = format!("{ROOT}{}.{}.", o.parent, o.kind);
                    self.queue(
                        device,
                        "GetParameterNames",
                        json!({"path":path,"next_level":true}),
                        WanPhase::VerifyDelete,
                    );
                    return false;
                }
                WanPhase::VerifyDelete => {
                    let id = self.current.as_ref().unwrap().0.clone();
                    let o = &self.owned[&id];
                    let path = format!("{ROOT}{}.{}.{}.", o.parent, o.kind, o.connection);
                    let exists = v
                        .result()
                        .rows()
                        .is_some_and(|a| a.iter().any(|p| p["name"] == path));
                    if exists {
                        self.fail(
                            device,
                            ItemOutcome::Unknown,
                            "DeleteObject accepted but WAN still exists; no replay",
                        );
                        return false;
                    }
                    if self.replacing.remove(&id) {
                        let baseline = self.parents.clone();
                        let o = self.owned.get_mut(&id).unwrap();
                        o.kind = ip_kind();
                        o.connection = 0;
                        o.state = WanOwnershipState::AddingChild;
                        o.baseline = Some(baseline);
                        let prefix = format!("{ROOT}{}.WANIPConnection.", o.parent);
                        self.queue(
                            device,
                            "AddObject",
                            json!({"object":prefix}),
                            WanPhase::AddingChild,
                        );
                        return false;
                    }
                    let path = format!("{ROOT}{}.", o.parent);
                    self.queue(
                        device,
                        "GetParameterNames",
                        json!({"path":path,"next_level":false}),
                        WanPhase::EmptyParent,
                    );
                    return false;
                }
                WanPhase::EmptyParent => {
                    let id = self.current.as_ref().unwrap().0.clone();
                    let parent = self.owned[&id].parent;
                    let prefix = format!("{ROOT}{parent}.");
                    let children = v.result().rows().into_iter().flatten().any(|p| {
                        p["name"]
                            .as_str()
                            .and_then(|n| n.strip_prefix(&prefix))
                            .is_some_and(|n| {
                                let parts: Vec<_> = n.split('.').collect();
                                parts.len() > 1
                                    && matches!(parts[0], "WANIPConnection" | "WANPPPConnection")
                                    && parts[1].parse::<u32>().is_ok()
                            })
                    });
                    if children {
                        self.owned.remove(&id);
                        self.results.insert(id,json!({"state":"deleted","reason":"surplus connection absence verified; shared parent retained","task_id":task}));
                        self.current = None;
                        self.stage = WanPhase::Next;
                        return false;
                    }
                    self.queue(
                        device,
                        "DeleteObject",
                        json!({"object":prefix}),
                        WanPhase::DeletingParent,
                    );
                    return false;
                }
                WanPhase::DeletingParent => {
                    self.queue(
                        device,
                        "GetParameterNames",
                        json!({"path":ROOT,"next_level":true}),
                        WanPhase::VerifyParentDelete,
                    );
                    return false;
                }
                WanPhase::VerifyParentDelete => {
                    let id = self.current.as_ref().unwrap().0.clone();
                    let parent = self.owned[&id].parent;
                    let path = format!("{ROOT}{parent}.");
                    if v.result()
                        .rows()
                        .is_none_or(|a| a.iter().any(|p| p["name"] == path))
                    {
                        self.fail(
                            device,
                            ItemOutcome::Unknown,
                            "parent deletion not confirmed; no replay",
                        );
                        return false;
                    }
                    self.parents.remove(&parent);
                    self.owned.remove(&id);
                    self.results.insert(id,json!({"state":"deleted","reason":"surplus connection and empty parent absence verified","task_id":task}));
                    self.current = None;
                    self.stage = WanPhase::Next;
                }
                WanPhase::Waiting | WanPhase::Next | WanPhase::Done => (),
            }
        }
        if self.current.is_none() {
            let Some((id, delete)) = self.actions.pop_front() else {
                if apply {
                    self.order_mapping(device);
                }
                self.stage = WanPhase::Done;
                return true;
            };
            self.current = Some((id.clone(), delete));
            if delete {
                let Some(o) = self.owned.get(&id) else {
                    self.current = None;
                    return false;
                };
                if o.state != WanOwnershipState::Ready {
                    self.fail(
                        device,
                        ItemOutcome::Unknown,
                        "owned WAN lifecycle interrupted; automatic deletion withheld",
                    );
                    return false;
                }
                let names = classification_names(
                    device,
                    o.parent,
                    o.connection,
                    &o.kind,
                    &self.evidence_paths,
                );
                self.queue(
                    device,
                    "GetParameterValues",
                    json!({"names":names}),
                    WanPhase::DeleteGuard,
                );
                return false;
            }
            if let Some(o) = self.owned.get(&id) {
                if o.state == WanOwnershipState::Ready {
                    self.remap(device, &id);
                    self.current = None;
                    return false;
                }
                if o.parent > 0 && o.connection == 0 && self.parents.contains(&o.parent) {
                    let path = format!("{ROOT}{}.", o.parent);
                    self.queue(
                        device,
                        "GetParameterNames",
                        json!({"path":path,"next_level":false}),
                        WanPhase::ChildNames,
                    );
                    return false;
                }
                self.fail(
                    device,
                    ItemOutcome::Unknown,
                    "WAN creation interrupted; automatic AddObject replay withheld",
                );
                return false;
            }
            let item = device.items.iter().find(|i| i.id == id).unwrap();
            if item.service_type == "tr069" {
                self.fail(device, ItemOutcome::Blocked, "TR069 is owned by OMCI");
                return false;
            }
            if !apply {
                self.fail(
                    device,
                    ItemOutcome::Drift,
                    "WAN missing; apply will allocate parent and IP connection",
                );
                return false;
            }
            let service_list = item
                .parameters
                .iter()
                .find(|p| p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST"))
                .map(|p| p.value.clone())
                .unwrap_or_default();
            self.owned.insert(
                id,
                Owned {
                    parent: 0,
                    connection: 0,
                    state: WanOwnershipState::AddingParent,
                    service_list,
                    kind: ip_kind(),
                    baseline: Some(self.parents.clone()),
                },
            );
            self.queue(
                device,
                "AddObject",
                json!({"object":ROOT}),
                WanPhase::AddingParent,
            );
        }
        false
    }
    fn discover_next(&mut self, device: &mut Device) {
        while let Some((parent, child, kind)) = self.discovery.front() {
            if !self
                .protected_roots
                .contains(&format!("{ROOT}{parent}.{kind}.{child}."))
            {
                break;
            }
            self.discovered.push(Owned {
                parent: *parent,
                connection: *child,
                kind: kind.clone(),
                state: WanOwnershipState::Protected,
                service_list: String::new(),
                baseline: None,
            });
            self.discovery.pop_front();
        }
        if let Some((parent, child, kind)) = self.discovery.front() {
            let names = classification_names(device, *parent, *child, kind, &self.evidence_paths);
            self.queue(
                device,
                "GetParameterValues",
                json!({"names":names}),
                WanPhase::Classifying,
            );
            return;
        }
        let editable: Vec<_> = self
            .discovered
            .iter()
            .filter(|o| o.state != WanOwnershipState::Protected)
            .cloned()
            .collect();
        if self.final_started {
            let expected: Vec<_> = device
                .items
                .iter()
                .filter(|i| i.kind == ItemKind::DeviceService && i.auto_instance.is_some())
                .filter_map(|i| self.owned.get(&i.id))
                .collect();
            let matched = expected.len() == editable.len()
                && expected.iter().zip(&editable).all(|(a, b)| {
                    a.parent == b.parent && a.connection == b.connection && a.kind == b.kind
                });
            self.results.insert("final_inventory".into(),json!({"state":if matched {"verified"} else if self.final_apply {"blocked"} else {"drift"},"reason":if matched {"full WAN inventory matches ordered configuration"} else {"full WAN inventory differs after reconciliation"}}));
            self.stage = WanPhase::Done;
            return;
        }
        let previous = std::mem::take(&mut self.owned);
        self.actions.clear();
        let desired: Vec<_> = device
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::DeviceService && i.auto_instance.is_some())
            .map(|i| i.id.clone())
            .collect();
        for (index, id) in desired.iter().enumerate() {
            let item = device.items.iter().find(|i| i.id == *id).unwrap();
            let service = item
                .parameters
                .iter()
                .find(|p| p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST"))
                .map(|p| p.value.as_str())
                .unwrap_or("");
            let contract = device.contract().wan;
            let untagged = contract.tagged(|name| {
                item.parameters
                    .iter()
                    .find(|p| p.name.ends_with(&format!(".{name}")))
                    .map(|p| p.value.clone())
            }) == Some(false);
            if untagged && protected(service, false) {
                self.results.insert(id.clone(),json!({"state":"blocked","reason":"untagged INTERNET is reserved for the fallback network"}));
                continue;
            }
            if let Some(old) = previous
                .get(id)
                .filter(|o| o.state != WanOwnershipState::Ready)
            {
                let mut old = old.clone();
                if old.parent == 0 {
                    let candidates: Vec<_> = old
                        .baseline
                        .as_ref()
                        .map(|b| self.parents.difference(b).copied().collect())
                        .unwrap_or_default();
                    if candidates.len() == 1
                        && !self.discovered.iter().any(|o| o.parent == candidates[0])
                    {
                        old.parent = candidates[0];
                    } else {
                        self.owned.insert(id.clone(), old);
                        self.results.insert(id.clone(),json!({"state":"unknown","reason":"interrupted parent creation cannot be uniquely recovered; AddObject replay withheld"}));
                        continue;
                    }
                }
                if self.parents.contains(&old.parent)
                    && !self.discovered.iter().any(|o| o.parent == old.parent)
                {
                    old.connection = 0;
                    old.kind = ip_kind();
                    old.baseline = Some(self.parents.clone());
                    self.owned.insert(id.clone(), old);
                    self.actions.push_back((id.clone(), false));
                    continue;
                }
            }
            if let Some(o) = editable.get(index) {
                if contract.vlan.location == crate::cwmp::contract::Location::Parent
                    && self
                        .discovered
                        .iter()
                        .filter(|p| p.parent == o.parent)
                        .count()
                        > 1
                {
                    let item = device.items.iter().find(|i| i.id == *id).unwrap();
                    let vlan = item
                        .parameters
                        .iter()
                        .find(|p| p.name.ends_with(contract.vlan.name))
                        .and_then(|p| p.value.parse::<u32>().ok());
                    let enabled = item
                        .parameters
                        .iter()
                        .find(|p| contract.enabled.is_some_and(|f| p.name.ends_with(f.name)))
                        .map(|p| matches!(p.value.as_str(), "1" | "true"));
                    let priority = item
                        .parameters
                        .iter()
                        .find(|p| p.name.ends_with(contract.priority.name))
                        .and_then(|p| p.value.parse::<u32>().ok());
                    if vlan.is_none()
                        || enabled.is_none()
                        || priority.is_none()
                        || self.vlans.get(&o.parent).copied() != vlan.zip(enabled)
                        || self.priorities.get(&o.parent).copied() != priority
                    {
                        self.results.insert(id.clone(),json!({"state":"blocked","reason":"requested parent VLAN conflicts with a shared WAN parent; other connections must be preserved"}));
                        continue;
                    }
                }
                if o.kind != "WANIPConnection" {
                    self.owned.insert(id.clone(), o.clone());
                    if self.prune {
                        self.replacing.insert(id.clone());
                        self.actions.push_back((id.clone(), true));
                    } else {
                        self.results.insert(id.clone(),json!({"state":"blocked","reason":"PPP conversion requires full-device synchronization"}));
                    }
                    continue;
                }
                self.owned.insert(id.clone(), o.clone());
                self.remap(device, id);
            }
            self.actions.push_back((id.clone(), false));
        }
        if self.prune {
            for (index, o) in editable.iter().enumerate().skip(desired.len()).rev() {
                let id = format!("surplus_{index}");
                self.owned.insert(id.clone(), o.clone());
                self.actions.push_back((id, true));
            }
        }
        self.stage = WanPhase::Next;
    }
    fn remap(&self, device: &mut Device, id: &str) {
        let o = &self.owned[id];
        let contract = device.contract();
        for item in &mut device.items {
            if item.wan_ref.as_deref() == Some(id) && item.auto_instance.is_some() {
                if item.kind == ItemKind::DeviceService {
                    if let Some(intent) = &item.wan_intent {
                        let enable = item
                            .parameters
                            .iter()
                            .find(|p| p.name.ends_with(".Enable"))
                            .map(|p| p.value.clone());
                        let activation_separate = enable.is_none();
                        let objects = crate::cwmp::path::WanObjects {
                            parent: o.parent,
                            connection: o.connection,
                            kind: if o.kind == "WANIPConnection" {
                                crate::cwmp::path::ConnectionKind::Ip
                            } else {
                                crate::cwmp::path::ConnectionKind::Ppp
                            },
                        };
                        match crate::cwmp::compiler::compile_wan(
                            intent,
                            contract,
                            &objects,
                            &crate::cwmp::path::EndpointMap::default(),
                        ) {
                            Ok(mut plan) => {
                                if activation_separate {
                                    plan.parameters.retain(|p| !p.name.ends_with(".Enable"));
                                } else if let Some(value) = &enable {
                                    for p in plan
                                        .parameters
                                        .iter_mut()
                                        .filter(|p| p.name.ends_with(".Enable"))
                                    {
                                        p.value = value.clone();
                                    }
                                }
                                item.parameters = plan.parameters;
                                item.guard = (!plan.guard.is_empty()).then_some(plan.guard);
                            }
                            Err(reason) => item.blocked = Some(reason),
                        }
                        continue;
                    }
                }
                for p in &mut item.parameters {
                    p.name = remapped(&p.name, o);
                }
                if let Some(g) = &mut item.guard {
                    *g = remapped(g, o);
                }
            }
        }
    }
    fn order_mapping(&mut self, device: &mut Device) {
        let ids: Vec<_> = device
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::DeviceService && i.auto_instance.is_some())
            .map(|i| i.id.clone())
            .collect();
        let mut rows: Vec<_> = ids
            .iter()
            .filter_map(|id| self.owned.get(id).cloned())
            .collect();
        if rows.len() != ids.len()
            || rows.iter().any(|o| o.state != WanOwnershipState::Ready)
            || ids.iter().any(|id| self.results.contains_key(id))
        {
            return;
        }
        rows.sort_by(|a, b| {
            (a.parent, a.connection, &a.kind).cmp(&(b.parent, b.connection, &b.kind))
        });
        for (id, row) in ids.into_iter().zip(rows) {
            self.owned.insert(id.clone(), row);
            self.remap(device, &id);
        }
    }
    pub fn block_unresolved(&self, device: &mut Device) {
        let global = self
            .results
            .get("inventory")
            .or_else(|| self.results.get("persistence"));
        for item in &mut device.items {
            if let Some(id) = &item.wan_ref {
                if item.auto_instance.is_some() && self.results.contains_key(id) {
                    item.blocked = Some(
                        self.results[id]["reason"]
                            .as_str()
                            .unwrap_or("WAN reconciliation failed")
                            .into(),
                    );
                } else if let Some(result) = global {
                    item.blocked = Some(
                        result["reason"]
                            .as_str()
                            .unwrap_or("WAN discovery failed")
                            .into(),
                    );
                }
            }
        }
    }
    pub fn save(&self, id: &str) -> Result<(), String> {
        let protected_parents: BTreeSet<_> = self
            .discovered
            .iter()
            .filter(|o| o.state == WanOwnershipState::Protected)
            .map(|o| o.parent)
            .chain(self.protected_roots.iter().filter_map(|root| {
                root.strip_prefix(ROOT)?
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            }))
            .collect();
        let text = json!({"cwmp_id":id,"owned":self.owned,"protected_parents":protected_parents})
            .to_string();
        let path = registry_path(id);
        super::storage::write_atomic(&path, text.as_bytes(), true)
            .map_err(|e| format!("persist WAN ownership: {e}"))
    }
}
fn classification_names(
    device: &Device,
    parent: u32,
    child: u32,
    kind: &str,
    evidence: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let root = format!("{ROOT}{parent}.");
    let path = format!("{root}{kind}.{child}.");
    device
        .contract()
        .wan
        .classification_paths(&root, &path)
        .into_iter()
        .chain(evidence.get(&path).into_iter().flatten().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn remapped(name: &str, o: &Owned) -> String {
    let Some((_, tail)) = name.strip_prefix(ROOT).and_then(|n| n.split_once('.')) else {
        return name.into();
    };
    let tail = if let Some(rest) = tail.strip_prefix("WANIPConnection.") {
        rest.split_once('.')
            .map(|(_, parameter)| format!("WANIPConnection.{}.{parameter}", o.connection))
            .unwrap_or_else(|| tail.into())
    } else {
        tail.into()
    };
    format!("{ROOT}{}.{tail}", o.parent)
}

#[cfg(test)]
mod tests {
    use super::super::{
        config::{Item, Parameter},
        model::ParameterKind,
        RPC,
    };
    use super::*;
    fn device() -> Device {
        Device {
            id: "child".into(),
            optical_sn: "HWTC12345678".into(),
            cwmp_id: "id".into(),
            omci_adapter: "hwtc".into(),
            bootstrap_adapter: "manual-acs".into(),
            provisioning_adapter: "hwtc-hw".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            items: vec![Item {
                wlan_intent: None,
                binding_intent: None,
                id: "internet".into(),
                kind: ItemKind::DeviceService,
                service_type: "internet".into(),
                guard: Some(format!("{ROOT}1000.WANIPConnection.1.X_HW_SERVICELIST")),
                blocked: None,
                wan_ref: Some("internet".into()),
                auto_instance: Some(1000),
                wan_intent: None,
                parameters: vec![Parameter {
                    name: format!("{ROOT}1000.WANIPConnection.1.X_HW_SERVICELIST"),
                    value: "INTERNET".into(),
                    kind: ParameterKind::String,
                    secret: false,
                }],
            }],
            wifi_cleanup: "preserve".into(),
            wifi_preserve: BTreeSet::new(),
        }
    }
    fn responses(v: Vec<Value>) {
        RPC.with(|r| *r.borrow_mut() = (v.into(), Vec::new()));
    }
    #[test]
    fn save_retries_directory_barrier_after_rename_and_identical_content() {
        use super::super::storage::test_io::{self, Point};
        let d = device();
        let r = Reconciler::new(&d, BTreeMap::new(), true);
        test_io::fail_next(Point::DirectorySync);
        assert!(r.save(&d.cwmp_id).is_err());
        assert!(std::path::Path::new(&registry_path(&d.cwmp_id)).exists());
        test_io::events();
        test_io::fail_next(Point::DirectorySync);
        assert!(r.save(&d.cwmp_id).is_err());
        assert_eq!(
            test_io::events(),
            vec![Point::FileSync, Point::DirectorySync]
        );
        r.save(&d.cwmp_id).unwrap();
        assert_eq!(test_io::events()[0], Point::FileSync);
    }
    fn ok(parameters: Value) -> Value {
        json!({"state":"completed","result":{"parameters":parameters}})
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn auto_creation_uses_returned_parent_and_child_instances() {
        let mut d = device();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        responses(vec![
            json!({"busy":false}),
            json!({"id":1}),
            ok(json!([])),
            json!({"id":2}),
            json!({"state":"completed","result":{"instance_number":"7"}}),
            json!({"id":3}),
            ok(json!([])),
            json!({"id":4}),
            json!({"state":"completed","result":{"instance_number":"3"}}),
        ]);
        for _ in 0..5 {
            r.tick(&mut d, true, false, true);
        }
        assert_eq!(r.owned["internet"].parent, 7);
        assert_eq!(r.owned["internet"].connection, 3);
        assert_eq!(
            d.items[0].parameters[0].name,
            format!("{ROOT}7.WANIPConnection.3.X_HW_SERVICELIST")
        );
    }
    #[cfg(feature = "device-hwtc")]
    #[test]
    fn resolved_intent_is_recompiled_without_bypassing_separate_activation() {
        use super::super::model::{ConnectionMode, VlanMode, WanIntent};
        let mut d = device();
        d.items[0].wan_intent = Some(WanIntent {
            services: vec!["INTERNET".into()],
            vlan: VlanMode::Tagged(66),
            priority: 6,
            mode: ConnectionMode::Bridge,
            bindings: vec![],
            internet: true,
            enabled: true,
        });
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.owned.insert(
            "internet".into(),
            Owned {
                parent: 7,
                connection: 3,
                state: WanOwnershipState::Ready,
                service_list: "INTERNET".into(),
                kind: ip_kind(),
                baseline: None,
            },
        );
        r.remap(&mut d, "internet");
        assert!(d.items[0]
            .parameters
            .iter()
            .all(|p| p.name.starts_with(&format!("{ROOT}7.WANIPConnection.3."))));
        assert!(d.items[0]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".X_HW_VLAN") && p.value == "66"));
        assert!(!d.items[0]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".Enable")));
        d.items[0].parameters.push(Parameter {
            name: format!("{ROOT}7.WANIPConnection.3.Enable"),
            value: "0".into(),
            kind: ParameterKind::Boolean,
            secret: false,
        });
        r.owned.get_mut("internet").unwrap().parent = 9;
        r.remap(&mut d, "internet");
        assert!(d.items[0]
            .parameters
            .iter()
            .any(|p| p.name == format!("{ROOT}9.WANIPConnection.3.Enable") && p.value == "0"));
        r.owned.get_mut("internet").unwrap().kind = "WANPPPConnection".into();
        r.remap(&mut d, "internet");
        assert!(d.items[0].blocked.is_some());
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn plan_allocates_nothing_and_unknown_add_is_not_replayed() {
        let mut d = device();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        responses(vec![json!({"busy":false}), json!({"id":1}), ok(json!([]))]);
        r.tick(&mut d, false, false, true);
        r.tick(&mut d, false, false, true);
        assert_eq!(r.results["internet"]["state"], "drift");
        assert!(r.owned.is_empty());
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        responses(vec![
            json!({"busy":false}),
            json!({"id":1}),
            ok(json!([])),
            json!({"id":2}),
            json!({"state":"unknown"}),
        ]);
        for _ in 0..3 {
            r.tick(&mut d, true, false, true);
        }
        assert_eq!(r.results["internet"]["state"], "unknown");
        assert_eq!(r.owned["internet"].parent, 0);
    }
    #[test]
    fn cross_contract_management_roots_are_not_reclassified_or_pruned() {
        let mut d = device();
        d.items.clear();
        d.provisioning_adapter = "hl202-cu".into();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.protected_roots
            .insert(format!("{ROOT}1.WANIPConnection.1."));
        r.discovery.push_back((1, 1, ip_kind()));
        responses(vec![]);
        r.discover_next(&mut d);
        assert_eq!(r.discovered[0].state, WanOwnershipState::Protected);
        assert!(r.actions.is_empty());
        assert!(r.owned.is_empty());
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn pruning_refuses_a_management_or_reassigned_wan() {
        let mut d = device();
        d.items.clear();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        responses(vec![
            json!({"busy":false}),
            json!({"id":1}),
            ok(json!([{"name":format!("{ROOT}2.WANIPConnection.1.")}])),
            json!({"id":2}),
            ok(
                json!([{"name":format!("{ROOT}2.WANIPConnection.1.X_HW_SERVICELIST"),"value":"TR069,INTERNET"},{"name":format!("{ROOT}2.WANIPConnection.1.X_HW_VLAN"),"value":"46"}]),
            ),
        ]);
        for _ in 0..3 {
            r.tick(&mut d, true, false, true);
        }
        assert!(r.owned.is_empty());
        assert_eq!(r.discovered[0].state, WanOwnershipState::Protected);
        RPC.with(|r| assert!(r.borrow().1.iter().all(|v| v["method"] != "DeleteObject")));
    }
    #[test]
    fn fallback_classification_is_semantic() {
        assert!(protected("INTERNET", false));
        assert!(!protected("INTERNET", true));
        assert!(protected("TR069,INTERNET", true));
        assert!(!protected("NOTTR069", true));
        assert!(!protected("OTHER", false));
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn discovery_replaces_stale_registry_and_preserves_order() {
        let mut d = device();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.discovered = vec![
            Owned {
                parent: 7,
                connection: 3,
                state: WanOwnershipState::Protected,
                service_list: "INTERNET".into(),
                kind: ip_kind(),
                baseline: None,
            },
            Owned {
                parent: 12,
                connection: 2,
                state: WanOwnershipState::Ready,
                service_list: "OTHER".into(),
                kind: ip_kind(),
                baseline: None,
            },
        ];
        r.discover_next(&mut d);
        assert_eq!(r.owned["internet"].parent, 12);
        assert_eq!(r.owned["internet"].connection, 2);
        assert!(d.items[0].parameters[0]
            .name
            .contains("12.WANIPConnection.2."));
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn shared_protected_parent_blocks_mutation() {
        let mut d = device();
        d.omci_adapter = "hl202".into();
        d.provisioning_adapter = "hl202-cu".into();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.discovered = vec![
            Owned {
                parent: 4,
                connection: 1,
                state: WanOwnershipState::Protected,
                service_list: "TR069".into(),
                kind: ip_kind(),
                baseline: None,
            },
            Owned {
                parent: 4,
                connection: 2,
                state: WanOwnershipState::Ready,
                service_list: "OTHER".into(),
                kind: ip_kind(),
                baseline: None,
            },
        ];
        r.discover_next(&mut d);
        r.block_unresolved(&mut d);
        assert!(d.items[0].blocked.is_some());
        assert!(r.actions.is_empty());
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn interrupted_add_recovers_only_a_unique_empty_parent() {
        let mut d = device();
        let old = Owned {
            parent: 0,
            connection: 0,
            state: WanOwnershipState::AddingParent,
            service_list: "INTERNET".into(),
            kind: ip_kind(),
            baseline: Some([1, 2, 3].into()),
        };
        let mut r = Reconciler::new(&d, [("internet".into(), old.clone())].into(), true);
        r.parents = [1, 2, 3, 4].into();
        r.discover_next(&mut d);
        assert_eq!(r.owned["internet"].parent, 4);
        assert_eq!(r.actions.len(), 1);
        let mut r = Reconciler::new(&d, [("internet".into(), old)].into(), true);
        r.parents = [1, 2, 3, 4, 5].into();
        r.discover_next(&mut d);
        assert!(r.actions.is_empty());
        assert_eq!(r.results["internet"]["state"], "unknown");
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn surplus_deletes_child_then_only_an_empty_parent() {
        let mut d = device();
        d.items.clear();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.discovered = vec![Owned {
            parent: 4,
            connection: 2,
            state: WanOwnershipState::Ready,
            service_list: "OTHER".into(),
            kind: ip_kind(),
            baseline: None,
        }];
        r.parents.insert(4);
        r.initialized = true;
        r.discover_next(&mut d);
        responses(vec![
            json!({"busy":false}),
            json!({"id":1}),
            ok(
                json!([{"name":format!("{ROOT}4.WANIPConnection.2.X_HW_SERVICELIST"),"value":"OTHER"},{"name":format!("{ROOT}4.WANIPConnection.2.X_HW_VLAN"),"value":"43"}]),
            ),
            json!({"id":2}),
            ok(json!([])),
            json!({"id":3}),
            ok(json!([])),
            json!({"id":4}),
            ok(json!([])),
            json!({"id":5}),
            ok(json!([])),
            json!({"id":6}),
            ok(json!([])),
        ]);
        for _ in 0..7 {
            r.tick(&mut d, true, false, true);
        }
        assert_eq!(r.results["surplus_0"]["state"], "deleted");
        assert!(r.owned.is_empty());
        RPC.with(|r| {
            let b = r.borrow();
            let deletes: Vec<_> =
                b.1.iter()
                    .filter(|v| v["method"] == "DeleteObject")
                    .map(|v| v["args"]["object"].as_str().unwrap())
                    .collect();
            assert_eq!(
                deletes,
                vec![format!("{ROOT}4.WANIPConnection.2."), format!("{ROOT}4.")]
            );
        });
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn final_inventory_detects_an_extra_connection() {
        let mut d = device();
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        let o = Owned {
            parent: 4,
            connection: 2,
            state: WanOwnershipState::Ready,
            service_list: "OTHER".into(),
            kind: ip_kind(),
            baseline: None,
        };
        r.discovered = vec![o.clone()];
        r.discover_next(&mut d);
        r.begin_final_verification(true);
        r.discovered.push(Owned { parent: 5, ..o });
        r.discover_next(&mut d);
        assert_eq!(r.results["final_inventory"]["state"], "blocked");
        assert_eq!(r.owned["internet"].parent, 4);
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn reused_instance_holes_are_remapped_in_numeric_order() {
        let mut d = device();
        let mut second = d.items[0].clone();
        second.id = "second".into();
        second.wan_ref = Some("second".into());
        second.auto_instance = Some(1001);
        second.parameters[0].name = second.parameters[0].name.replace("1000", "1001");
        d.items.push(second);
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        let row = Owned {
            parent: 7,
            connection: 3,
            state: WanOwnershipState::Ready,
            service_list: "OTHER".into(),
            kind: ip_kind(),
            baseline: None,
        };
        r.owned.insert("internet".into(), row.clone());
        r.remap(&mut d, "internet");
        r.owned.insert(
            "second".into(),
            Owned {
                parent: 4,
                connection: 2,
                ..row
            },
        );
        r.remap(&mut d, "second");
        r.order_mapping(&mut d);
        assert!(d.items[0].parameters[0]
            .name
            .contains("4.WANIPConnection.2."));
        assert!(d.items[1].parameters[0]
            .name
            .contains("7.WANIPConnection.3."));
    }

    #[test]
    fn failed_wan_or_binding_preflight_prevents_lifecycle_rpcs() {
        for binding in [false, true] {
            let mut d = device();
            if binding {
                let mut item = d.items[0].clone();
                item.id = "binding".into();
                item.kind = ItemKind::PortBinding;
                item.blocked = Some("binding contract is unverified".into());
                d.items.push(item);
            } else {
                d.items[0].blocked = Some("service encoding is unverified".into());
                d.items[0].parameters.clear();
            }
            let mut reconciler = Reconciler::new(&d, BTreeMap::new(), true);
            super::super::controller::RPC.with(|r| *r.borrow_mut() = (VecDeque::new(), Vec::new()));
            assert!(reconciler.tick(&mut d, true, false, true));
            assert_eq!(reconciler.results["inventory"]["state"], "blocked");
            super::super::controller::RPC.with(|r| assert!(r.borrow().1.is_empty()));
            assert!(reconciler.owned.is_empty());
        }
    }

    #[test]
    fn preflight_does_not_discard_an_inflight_lifecycle_task() {
        let mut d = device();
        d.items[0].blocked = Some("later item failure".into());
        let mut r = Reconciler::new(&d, BTreeMap::new(), true);
        r.initialized = true;
        r.stage = WanPhase::AddingParent;
        r.task = Some(7);
        responses(vec![json!({"state":"sent"})]);
        assert!(!r.tick(&mut d, true, false, true));
        assert_eq!(r.task, Some(7));
        assert_eq!(r.stage, WanPhase::AddingParent);
        super::super::controller::RPC.with(|rpc| {
            assert_eq!(rpc.borrow().1, vec![json!({"op":"task","id":7})]);
        });
    }
}
use crate::cwmp::transport::Command;
