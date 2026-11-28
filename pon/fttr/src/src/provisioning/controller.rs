//! Desired-state controller. CWMP traffic runs independently of optical workers.
use super::model::{
    Backend, ItemKind, ItemOutcome, OperationPhase, ParameterKind, SharedConfig, TaskOutcome,
};
use super::{config, wan, wifi};
#[path = "controller/admission.rs"]
mod admission;
#[path = "controller/facts.rs"]
mod facts;
#[path = "controller/lifecycle.rs"]
mod lifecycle;
#[cfg(test)]
#[path = "controller/lifecycle_tests.rs"]
mod lifecycle_tests;
mod observation;
#[path = "controller/operation.rs"]
mod operation;
#[path = "parameters.rs"]
mod parameters;
#[cfg(test)]
#[path = "controller/persistence_tests.rs"]
mod persistence_tests;
#[path = "controller/reset.rs"]
mod reset;
#[path = "controller/scheduler.rs"]
mod scheduler;
#[cfg(test)]
#[path = "controller/scheduling_tests.rs"]
mod scheduling_tests;
#[path = "controller/snapshots.rs"]
mod snapshots;
use crate::cwmp::observations::{DeviceObservations, ListenerObservation};
use crate::cwmp::transport::Command;
use config::{Config, Device, Item, Parameter};
use parameters::advance;
#[cfg(test)]
use parameters::equivalent;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn error(s: &str) -> Value {
    json!({"error":s})
}
#[cfg(all(feature = "acs", not(test)))]
pub(super) fn acs(v: Command) -> crate::cwmp::transport::Reply {
    crate::acs::transact(v)
}
#[cfg(all(not(feature = "acs"), not(test)))]
pub(super) fn acs(_: Command) -> crate::cwmp::transport::Reply {
    error("ACS adapter not compiled").into()
}
#[cfg(test)]
thread_local! { pub(super) static RPC: std::cell::RefCell<(std::collections::VecDeque<Value>,Vec<Value>)> = std::cell::RefCell::new((std::collections::VecDeque::new(),Vec::new())); }
#[cfg(test)]
pub(super) fn acs(v: Command) -> crate::cwmp::transport::Reply {
    RPC.with(|r| {
        let mut r = r.borrow_mut();
        r.1.push(v.wire());
        r.0.pop_front().expect("unexpected RPC").into()
    })
}

static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
#[derive(Default)]
struct Ready {
    all: bool,
    devices: BTreeSet<String>,
}
static READY: OnceLock<Mutex<Ready>> = OnceLock::new();
fn ready() -> &'static Mutex<Ready> {
    READY.get_or_init(Mutex::default)
}
pub(crate) fn wake() {
    ready().lock().unwrap().all = true;
    WAKE.notify_one();
}
pub(crate) fn wake_device(device: &str) {
    let mut r = ready().lock().unwrap();
    if r.devices.len() < 128 {
        r.devices.insert(device.to_owned());
    } else {
        r.all = true;
    }
    WAKE.notify_one();
}
pub(crate) fn external_write_allowed(serial: &str) -> bool {
    let c = controller().lock().unwrap();
    c.config
        .devices
        .iter()
        .find(|d| d.optical_sn == serial)
        .is_some_and(|d| !c.unresolved_owner(d) && !c.resets.blocks(serial))
        && !c.snapshots.as_ref().is_some_and(|s| s.blocked())
}

struct AcsExecution {
    submission: Option<crate::cwmp::transport::Pending<OperationPhase>>,
    verify_after: u64,
    compiled: bool,
    capabilities: crate::cwmp::runtime::Discovery,
    index: usize,
    task: Option<u64>,
    roots: Vec<String>,
    writable: BTreeSet<String>,
    changes: Vec<Parameter>,
    uncertain: bool,
    pending: bool,
    written: bool,
    credential_write: Option<TaskOutcome>,
    passphrase_fallback: bool,
    wan: wan::Reconciler,
    wifi: wifi::Reconciler,
}
#[derive(Default)]
struct CommandExecution {
    job: Option<crate::provisioning::command::Job>,
    reservation: usize,
    result: Option<Value>,
    observes: Option<u64>,
    reboot_from: Option<String>,
    recovers: Option<u64>,
}
enum Execution {
    Acs(Box<AcsExecution>),
    Command(CommandExecution),
    Disabled,
}
struct Operation {
    execution: Execution,
    work_started: bool,
    full_sync: bool,
    id: u64,
    device: Device,
    revision: String,
    apply: bool,
    management: bool,
    stage: OperationPhase,
    results: BTreeMap<String, Value>,
    started: u64,
    completed_at: Option<u64>,
}
// CWMP transitions use the ACS payload only after backend dispatch. Native
// operations never allocate discovery/reconcilers or carry a second executor.
impl std::ops::Deref for Operation {
    type Target = AcsExecution;
    fn deref(&self) -> &AcsExecution {
        match &self.execution {
            Execution::Acs(state) => state,
            _ => panic!("CWMP transition on a non-ACS operation"),
        }
    }
}
impl std::ops::DerefMut for Operation {
    fn deref_mut(&mut self) -> &mut AcsExecution {
        match &mut self.execution {
            Execution::Acs(state) => state,
            _ => panic!("CWMP transition on a non-ACS operation"),
        }
    }
}
impl Operation {
    fn command(&self) -> Option<&CommandExecution> {
        match &self.execution {
            Execution::Command(state) => Some(state),
            _ => None,
        }
    }
    fn command_mut(&mut self) -> Option<&mut CommandExecution> {
        match &mut self.execution {
            Execution::Command(state) => Some(state),
            _ => None,
        }
    }
    fn command_running(&self) -> bool {
        self.command().is_some_and(|state| state.job.is_some())
    }
    fn observes(&self) -> Option<u64> {
        self.command().and_then(|state| state.observes)
    }
    fn recovers(&self) -> Option<u64> {
        self.command().and_then(|state| state.recovers)
    }
    fn unstarted(&self) -> bool {
        !self.work_started
            && match &self.execution {
                Execution::Acs(state) => {
                    state.index == 0 && state.task.is_none() && state.submission.is_none()
                }
                Execution::Command(state) => state.job.is_none(),
                Execution::Disabled => true,
            }
    }
}
#[derive(Default)]
struct Controller {
    resources: Option<&'static crate::platform::resources::Resources>,
    pub(super) dirty: bool,
    resets: reset::Resets,
    config: SharedConfig,
    pending_config: Option<(Config, u64)>,
    failure: Option<String>,
    config_error: Option<String>,
    next_id: u64,
    operations: BTreeMap<String, Operation>,
    management: BTreeMap<String, Value>,
    mqtt: BTreeMap<String, Value>,
    last_online: BTreeSet<String>,
    startup_pending: BTreeSet<String>,
    last_boot: BTreeMap<String, u64>,
    optical_epochs: BTreeMap<String, (u16, u64)>,
    recovered: BTreeMap<String, Value>,
    observations: BTreeMap<String, Value>,
    retired: Vec<Value>,
    saved: String,
    owned: BTreeMap<String, BTreeMap<String, wan::Owned>>,
    wifi_owned: BTreeMap<String, wifi::Registry>,
    omci_management: BTreeMap<String, Value>,
    acs_status: ListenerObservation,
    attempts: BTreeMap<String, u32>,
    next_retry_at: BTreeMap<String, u64>,
    automatic_ready: BTreeMap<String, scheduler::Reasons>,
    read_recovery: BTreeMap<String, snapshots::ReadRecovery>,
    snapshots: Option<snapshots::Store>,
    command_cursor: Option<String>,
}
static CONTROLLER: OnceLock<Mutex<Controller>> = OnceLock::new();
fn controller() -> &'static Mutex<Controller> {
    CONTROLLER.get_or_init(|| Mutex::new(Controller::default()))
}
pub(crate) fn allows_generic_bridge(serial: &str) -> bool {
    let c = controller().lock().unwrap();
    if c.config.allow_unsupported_as_bridge {
        return true;
    }
    c.config
        .devices
        .iter()
        .any(|d| d.optical_sn.eq_ignore_ascii_case(serial) && d.omci_adapter == "generic")
}

pub(crate) fn configured_omci_adapter(serial: &str) -> Option<String> {
    let c = controller().lock().unwrap();
    c.config
        .devices
        .iter()
        .find(|d| d.optical_sn.eq_ignore_ascii_case(serial))
        .map(|d| d.omci_adapter.clone())
}

pub(crate) fn management_desired(serial: &str) -> Option<(config::Management, String)> {
    if !crate::omci_auth::allowed(serial) {
        return None;
    }
    if !controller()
        .lock()
        .unwrap()
        .config
        .devices
        .iter()
        .any(|d| d.optical_sn == serial && d.omci_management())
    {
        return None;
    }
    let (id, management, version, epoch, automatic) =
        super::management_network::omci_grant(serial)?;
    super::bootstrap::omci_write_allowed(&id, epoch, automatic).then_some((management, version))
}
pub(crate) fn management_result(serial: &str, result: Value) {
    let mut c = controller().lock().unwrap();
    if c.config.devices.iter().any(|d| d.optical_sn == serial) {
        c.omci_management.insert(serial.into(), result);
    }
}
impl Controller {
    fn omci_management_view(&self, device: &Device) -> Value {
        if !device.omci_management() {
            json!({"state":"disabled","enabled":false,"reason":"OMCI management provisioning is disabled for this device"})
        } else {
            let mut value = self
                .omci_management
                .get(&device.optical_sn)
                .cloned()
                .unwrap_or_else(|| json!({"state":"pending"}));
            value["enabled"] = json!(true);
            value
        }
    }
    fn forget_device(&mut self, serial: &str, cwmp_id: &str) {
        let device_id = self
            .config
            .devices
            .iter()
            .find(|d| d.optical_sn == serial)
            .map(|d| &d.id);
        self.operations
            .retain(|id, op| Some(id) != device_id && op.device.optical_sn != serial);
        self.recovered
            .retain(|id, op| Some(id) != device_id && op["optical_sn"] != serial);
        if let Some(id) = device_id {
            self.observations.remove(id);
            self.read_recovery.remove(id);
        }
        self.owned.remove(cwmp_id);
        self.wifi_owned.remove(cwmp_id);
        self.dirty = true;
    }
    fn startup_trigger_pending(&self, device: &str) -> bool {
        self.config.triggers.contains("program_start") && self.startup_pending.contains(device)
    }
    fn periodic_due(&self, device: &str, time: u64) -> bool {
        if self.config.interval_seconds == 0 || !self.config.triggers.contains("periodic") {
            return false;
        }
        let completed_at = if let Some(facts) = self.previous_facts(device) {
            if facts.phase != Some(OperationPhase::Completed) {
                return false;
            }
            facts.completed_at
        } else {
            // A device with no previous operation can run its first periodic sync.
            return true;
        };
        completed_at.is_some_and(|at| time.saturating_sub(at) >= self.config.interval_seconds)
    }
    fn start(&mut self, target: &str, apply: bool, management: bool) -> Result<Vec<u64>, String> {
        self.start_selected(target, apply, management, None)
    }
    fn start_selected(
        &mut self,
        target: &str,
        apply: bool,
        management: bool,
        item: Option<&str>,
    ) -> Result<Vec<u64>, String> {
        if self.snapshots.as_ref().is_some_and(|s| s.blocked()) {
            return Err("durable recovery state unavailable; repair snapshot storage before synchronization".into());
        }
        if self.pending_config.is_some() {
            return Err("configuration reload pending; new synchronization will use the latest configuration".into());
        }
        if apply && !self.config.enabled {
            return Err("provisioning disabled by UCI".into());
        }
        let mut devices: Vec<_> = self
            .config
            .devices
            .iter()
            .filter(|d| {
                target == "all"
                    || target == d.id
                    || target == d.optical_sn
                    || self
                        .config
                        .assignments
                        .get(&d.id)
                        .is_some_and(|(group, _)| target == format!("group:{group}"))
            })
            .cloned()
            .collect();
        if target != "all"
            && !target.starts_with("group:")
            && devices.iter().any(|d| d.backend() == "disabled")
        {
            return Err("business provisioning is disabled for this device".into());
        }
        devices.retain(|d| d.backend() != "disabled");
        if let Some(item) = item {
            if devices
                .iter()
                .any(|d| d.items.iter().any(|i| i.id == item && i.wan_ref.is_some()))
            {
                return Err(
                    "ordered WAN reconciliation requires full-device synchronization".into(),
                );
            }
            for d in &mut devices {
                d.items.retain(|i| i.id == item);
            }
            devices.retain(|d| !d.items.is_empty());
        }
        if devices.is_empty() {
            return Err("no configured device matches target".into());
        }
        if !cfg!(any(feature = "acs", test)) && devices.iter().any(|d| d.backend() == "acs") {
            return Err("ACS backend not compiled; select an available per-device backend".into());
        }
        if devices.iter().any(|d| self.resets.blocks(&d.optical_sn)) {
            return Err("device reset pending; retry reset before synchronization".into());
        }
        if apply
            && devices
                .iter()
                .any(|d| !crate::omci_auth::allowed(&d.optical_sn))
        {
            return Err("OMCI authentication is not authorized for the selected device".into());
        }
        if apply
            && self.config.management_network.as_ref().is_some_and(|d| {
                devices.iter().any(|device| device.backend() == "acs")
                    && !super::management_network::matches_desired(d)
            })
        {
            return Err(
                "apply the host management network before ACS business synchronization".into(),
            );
        }
        if devices.iter().any(|d| {
            self.operations.get(&d.id).is_some_and(|o| {
                o.stage != OperationPhase::Completed && o.stage != OperationPhase::Interrupted
            })
        }) {
            return Err("device already has an active operation".into());
        }
        // Until transaction-aware recovery is available, neither a plan nor a
        // new apply may replace the only record of an unresolved write.
        if devices.iter().any(|d| self.unresolved_owner(d)) {
            return Err("unresolved prior write; transaction-aware recovery required before a new plan or apply".into());
        }
        if apply && self.snapshots.is_some() {
            for device in &devices {
                super::runtime_store::approve(&device.optical_sn)
                    .map_err(|e| format!("record explicit write admission: {e}"))?;
            }
        }
        let mut ids = Vec::new();
        for d in devices {
            self.next_id += 1;
            ids.push(self.next_id);
            let mut op = Operation::new(
                self.next_id,
                d.clone(),
                self.config.device_revision(&d.id).to_owned(),
                apply,
                management,
            );
            op.full_sync = item.is_none();
            if d.backend() == "acs" {
                let owned = wan::load(&d.cwmp_id)?
                    .unwrap_or_else(|| self.owned.get(&d.cwmp_id).cloned().unwrap_or_default());
                op.wan = wan::Reconciler::new(&d, owned, item.is_none());
                op.wifi = wifi::Reconciler::new(&d, wifi::load(&d.cwmp_id)?, item.is_none());
            }
            if item.is_some() {
                let previous = self.operations.get(&d.id).map(Operation::view);
                if let Some(items) = previous
                    .as_ref()
                    .or_else(|| self.recovered.get(&d.id))
                    .and_then(|p| p["items"].as_array())
                {
                    for i in items {
                        if let Some(id) = i["id"].as_str() {
                            if !d.items.iter().any(|selected| selected.id == id) {
                                op.results.insert(id.into(), i.clone());
                            }
                        }
                    }
                }
            }
            if d.items.is_empty() {
                op.stage = OperationPhase::WanReconciling;
            }
            self.recovered.remove(&d.id);
            self.operations.insert(d.id.clone(), op);
            self.dirty = true;
        }
        Ok(ids)
    }
    fn snapshot(&self) -> Value {
        let mut status = self.business_snapshot();
        status["onu_pins"] = json!(self.config.onu_pins);
        status["config_error"] = json!(self.config_error);
        status["config_load"] = self.config_load_view();
        status["recovery_storage"] = self.snapshots.as_ref().map_or(Value::Null, |s| s.view());
        status["process_memory"] = crate::memory::view();
        status["mqtt_report_resources"] = crate::devices::mqtt_report_resources();
        status
    }

    fn business_snapshot(&self) -> Value {
        let devices: Vec<_> = self.config.devices.iter().map(|d| {
            let mut operation = self.business_operation(&d.id).map(Operation::view).or_else(|| self.recovered.get(&d.id).or_else(|| self.recovery_owner(d)).cloned()).unwrap_or_else(|| json!({"state":"not_started","items":d.items.iter().map(|i| json!({"id":i.id,"kind":i.kind,"service_type":i.service_type,"state":"not_started"})).collect::<Vec<_>>() }));
            if let Some(observation) = self.observation_view(d) {
                operation["recovery_observation"] = observation;
            }
            operation["backend"] = d.backend_view();
            if d.backend() == "disabled" {
                operation = json!({"state":"disabled","items":[],"backend":d.backend_view(),"reason":"business provisioning is disabled for this device"});
            }
            operation["desired_revision"] = json!(self.config.device_revision(&d.id));
            operation["current_revision"] = json!(operation["revision"] == self.config.device_revision(&d.id));
            if self.resets.blocks(&d.optical_sn) {
                operation = json!({"state":"reset_pending","items":[],"reason":self.resets.reason(&d.optical_sn)});
            }
            let management_fresh = self.business_ready(d);
            operation["management_fresh"] = json!(management_fresh);
            if let Some(attempts) = self.attempts.get(&d.id) {
                operation["attempts"] = json!(attempts);
            }
            if let Some(next_retry) = self.next_retry_at.get(&d.id) {
                operation["next_retry_at"] = json!(next_retry);
            }
            operation["read_recovery"] = json!(self.read_recovery.get(&d.id));
            operation["automatic_write_admission"] = json!(if self.snapshots.is_none() || super::runtime_store::approved(&d.optical_sn) { "approved" } else { "explicit_apply_required" });
            if !management_fresh && matches!(operation["state"].as_str(), Some("waiting" | "recovering")) {
                operation["state"] = json!("waiting_management");
            }
            if let Some(owned)=self.owned.get(&d.cwmp_id) { operation["wan_objects"]=json!(owned); }
            if let Some(owned)=self.wifi_owned.get(&d.cwmp_id) { operation["wifi_objects"]=json!(owned); }
            let previous=operation["items"].as_array().cloned().unwrap_or_default();
            let items: Vec<_>=d.items.iter().filter(|_| d.backend() != "disabled").map(|desired| {
                let mut item=previous.iter().find(|i| i["id"] == desired.id).cloned().unwrap_or_else(|| json!({"id":desired.id,"kind":desired.kind,"service_type":desired.service_type,"state":"not_started"}));
                item["name"]=json!(self.config.labels.get(desired.id.rsplit("__").next().unwrap_or(&desired.id)));
                item["current_revision"]=json!(item["revision"] == self.config.device_revision(&d.id));
                item["management_fresh"]=operation["management_fresh"].clone();
                if item.get("service_type").is_none() {
                    item["service_type"] = json!(desired.service_type);
                }
                item
            }).collect();
            operation["items"]=json!(items);
            let ssids: Vec<_> = items.iter().filter(|i| i["kind"] == "ssid").cloned().collect();
            let services: Vec<_> = items.iter().filter(|i| i["kind"] != "ssid").cloned().collect();
            let wifi_sync=if d.backend() == "disabled" { "disabled" } else if operation["wifi_lifecycle"].as_object().is_some_and(|m|m.values().any(|i|matches!(ItemOutcome::from_result(i),Some(ItemOutcome::Blocked|ItemOutcome::Failed|ItemOutcome::Unknown)))) {
                "attention_required"
            } else if operation["state"] == "recovering" || ssids.iter().any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::ReconcileRequired)) {
                "recovering"
            } else if operation["state"] == "waiting_management" || (!management_fresh && operation["state"] != "completed") {
                "waiting_management"
            } else if operation["wifi_stage"].is_string() && operation["wifi_stage"] != "done" {
                "pending"
            } else {
                aggregate(&ssids)
            };
            let device_facts = self
                .business_operation(&d.id)
                .map(Operation::facts)
                .or_else(|| {
                    self.recovered
                        .get(&d.id)
                        .or_else(|| self.recovery_owner(d))
                        .map(facts::Facts::recovered)
                });
            if d.backend() != "disabled" {
                let phase = match operation["state"].as_str() {
                    Some("discovering" | "reading" | "disabling_read") => "inventory",
                    Some("wan_reconciling" | "wifi_reconciling") => "planning",
                    Some("writing" | "disabling") => "writing",
                    Some("settling") => "settling",
                    Some("verifying" | "wan_verifying" | "recovering") => "verifying",
                    Some("completed" | "interrupted" | "disabled") => "finished",
                    _ => "queued",
                };
                operation["phase"] = json!(phase);
                if let Some(facts) = device_facts.as_ref() {
                    let outcome = if facts.unresolved {
                        "unknown"
                    } else if facts.failed {
                        "blocked"
                    } else if facts.drift {
                        "drift"
                    } else if facts.verified {
                        "verified"
                    } else if facts.reconcile {
                        "retryable_failure"
                    } else {
                        "pending"
                    };
                    operation["outcome"] = json!(outcome);
                    let rec_state = operation
                        .get("recovery_observation")
                        .and_then(|o| o["recovery_state"].as_str())
                        .unwrap_or_else(|| {
                            if facts.unresolved {
                                "manual_required"
                            } else if !management_fresh && operation["state"] != "completed" {
                                "waiting_transport"
                            } else if self.next_retry_at.get(&d.id).is_some_and(|&t| t > 0)
                                || self
                                    .read_recovery
                                    .get(&d.id)
                                    .is_some_and(|r| r.attempts > 0)
                            {
                                "retry_scheduled"
                            } else {
                                "none"
                            }
                        });
                    operation["recovery_state"] = json!(rec_state);
                    if let Some(action) = operation
                        .get("recovery_observation")
                        .and_then(|o| o["manual_action"].as_str())
                        .filter(|s| !s.is_empty())
                    {
                        operation["manual_action"] = json!(action);
                    }
                    if facts.verified {
                        if let Some(at) = operation["completed_at"].as_u64() {
                            operation["last_verified_at"] = json!(at);
                        }
                        if let Some(rev) = operation["revision"].as_str().filter(|s| !s.is_empty())
                        {
                            operation["verified_revision"] = json!(rev);
                        }
                    }
                    let affected: Vec<_> = items
                        .iter()
                        .filter(|i| {
                            matches!(
                                i["state"].as_str(),
                                Some(
                                    "unknown"
                                        | "blocked"
                                        | "failed"
                                        | "drift"
                                        | "reconcile_required"
                                )
                            )
                        })
                        .filter_map(|i| i["id"].as_str().map(str::to_owned))
                        .collect();
                    if !affected.is_empty() {
                        operation["affected_items"] = json!(affected);
                    }
                }
            }
            let assignment = self.config.assignments.get(&d.id);
            json!({"device":d.id,"name":self.config.labels.get(&d.id),"group":assignment.map(|a|&a.0),"group_name":assignment.and_then(|a|self.config.labels.get(&a.0)),"template":assignment.map(|a|&a.1),"template_name":assignment.and_then(|a|self.config.labels.get(&a.1)),"optical_sn":d.optical_sn,"omci_adapter":d.omci_adapter,"bootstrap_adapter":d.bootstrap_adapter,"provisioning_adapter":d.provisioning_adapter,"omci_auth":crate::omci_auth::view(&d.optical_sn),"omci_management":self.omci_management_view(d),"management":self.management.get(&d.id),"mqtt":self.mqtt.get(&d.id),"mqtt_bridge":crate::controller::mqtt_bridge::device_view(&d.optical_sn),"acs_bootstrap":super::bootstrap::view(d,self.config.management.as_ref(),&self.acs_status),"provisioning":operation,"services":services,"ssids":ssids,"wifi_sync":wifi_sync})
        }).collect();
        json!({"enabled":self.config.enabled,"mode":if self.config.automatic {"auto"} else {"manual"},"method":"diff","revision":self.config.revision,"triggers":self.config.triggers,"interval_minutes":self.config.interval_seconds / 60,"omci_management":{"desired":self.config.management,"state":"not_verified"},"management_network":super::management_network::view(self.config.management_network.as_ref()),"mqtt_bridge":crate::controller::mqtt_bridge::global_view(),"acs_bridge":crate::controller::acs_bridge::global_view(),"groups":self.config.groups.iter().map(|(id,t)|json!({"id":id,"name":self.config.labels.get(id),"template":t})).collect::<Vec<_>>(),"error":self.failure,"devices":devices,"pending_devices":devices.iter().filter(|d| !matches!(d["provisioning"]["state"].as_str(),Some("completed" | "disabled"))).map(|d| d["device"].clone()).collect::<Vec<_>>()})
    }

    fn business_ready(&self, device: &Device) -> bool {
        if !crate::omci_auth::allowed(&device.optical_sn) {
            return false;
        }
        match device.backend() {
            "command" => self
                .mqtt
                .get(&device.id)
                .is_some_and(|s| s["state"] == "online"),
            "acs" => {
                self.management
                    .get(&device.id)
                    .is_some_and(|s| s["state"] == "online")
                    && self
                        .config
                        .management_network
                        .as_ref()
                        .is_none_or(super::management_network::matches_desired)
            }
            _ => false,
        }
    }
}
fn discovered_identity<'a>(devices: &'a DeviceObservations, optical_sn: &str) -> Option<&'a str> {
    let mut matches = devices.iter().filter(|(_, d)| {
        crate::devices::matches_cwmp_identity(optical_sn, &d.product_class, &d.serial)
    });
    let (id, _) = matches.next()?;
    matches.next().is_none().then_some(id.as_str())
}
fn aggregate(items: &[Value]) -> &str {
    if items.is_empty() {
        "not_configured"
    } else if items
        .iter()
        .all(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::Verified))
    {
        if items.iter().any(|i| i["current_revision"] != true) {
            "stale_revision"
        } else if items.iter().any(|i| i["management_fresh"] != true) {
            "stale_management"
        } else {
            "verified"
        }
    } else if items.iter().any(|i| {
        matches!(
            ItemOutcome::from_result(i),
            Some(ItemOutcome::Failed | ItemOutcome::Blocked | ItemOutcome::Unknown)
        )
    }) {
        "attention_required"
    } else if items
        .iter()
        .any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::ReconcileRequired))
    {
        "recovering"
    } else if items
        .iter()
        .any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::AcceptedUnverified))
    {
        "accepted_unverified"
    } else {
        "pending"
    }
}
pub fn snapshot() -> Value {
    controller().lock().unwrap().snapshot()
}
#[cfg(feature = "acs")]
pub(crate) fn connection_request_devices() -> BTreeMap<String, super::model::ConnectionRequest> {
    let c = controller().lock().unwrap();
    c.config
        .devices
        .iter()
        .filter(|d| !c.resets.blocks(&d.optical_sn))
        .map(|d| (d.cwmp_id.clone(), c.connection_request_policy(d)))
        .collect()
}
impl Controller {
    fn connection_request_policy(&self, device: &Device) -> super::model::ConnectionRequest {
        let mut policy = device.connection_request.clone();
        if policy.defaults {
            if let Some(credentials) = self
                .management
                .get(&device.id)
                .and_then(|m| m["product_class"].as_str())
                .and_then(crate::devices::cwmp_credentials)
            {
                policy.username = credentials.username;
                policy.password = credentials.password;
                policy.defaults = false;
            }
        }
        policy
    }
}
impl Controller {
    fn operation_view(&self, id: u64) -> Option<Value> {
        self.operations
            .values()
            .find(|op| op.id == id)
            .map(Operation::view)
            .or_else(|| {
                self.recovered
                    .values()
                    .chain(self.observations.values())
                    .find(|op| op["operation_id"].as_u64() == Some(id))
                    .cloned()
            })
    }
}

pub fn control(request: &str) -> String {
    let mut changed = false;
    let reply = (|| -> Result<Value, String> {
        let v: Value = serde_json::from_str(request).map_err(|_| "invalid provisioning request")?;
        changed = matches!(
            v["op"].as_str(),
            Some(
                "reload"
                    | "reset_state"
                    | "management_plan"
                    | "management_apply"
                    | "plan"
                    | "apply"
                    | "reconcile"
                    | "observe"
                    | "recover"
                    | "recover_reboot"
                    | "retry"
            )
        );
        match v["op"].as_str().unwrap_or("") {
            "check" => {
                let cfg = config::load()?;
                if let Some(resources) = controller().lock().unwrap().resources {
                    lifecycle::validate_desired_pins(resources, &cfg)?;
                }
                Ok(json!({"valid":true,"revision":cfg.revision,"devices":cfg.devices.len()}))
            }
            "reload" => {
                let mut c = controller().lock().unwrap();
                let cfg = config::load().inspect_err(|error| {
                    // An invalid newer commit must not activate an older queued revision.
                    c.pending_config = None;
                    c.config_error = Some(error.clone());
                })?;
                c.request_config(cfg);
                // The scheduler compares each device's operation revision with
                // the desired revision. One unavailable device must not consume
                // the config-change trigger for every other device.
                Ok(c.snapshot())
            }
            "status" => Ok(snapshot()),
            "reset_state" => controller()
                .lock()
                .unwrap()
                .reset_device(v["target"].as_str().unwrap_or("")),
            "management_plan" | "management_apply" => {
                let cfg = config::load()?;
                let desired = cfg
                    .management_network
                    .clone()
                    .ok_or("configure the management IPv4 address/prefix first")?;
                let mut c = controller().lock().unwrap();
                c.replace_config(cfg)?;
                super::management_network::request(desired, v["op"] == "management_apply")
            }
            "operation" => {
                let c = controller().lock().unwrap();
                Ok(v["id"]
                    .as_u64()
                    .and_then(|id| c.operation_view(id))
                    .unwrap_or_else(|| error("unknown operation")))
            }
            "plan" | "apply" => {
                let mut c = controller().lock().unwrap();
                let ids = c.start_selected(
                    v["target"].as_str().unwrap_or("all"),
                    v["op"] == "apply",
                    v["management"].as_bool().unwrap_or(false),
                    v["item"].as_str(),
                )?;
                Ok(json!({"operation_ids":ids,"revision":c.config.revision}))
            }
            "reconcile" => {
                let target = v["target"].as_str().unwrap_or("all");
                let mut c = controller().lock().unwrap();
                let ids = c.start_selected(target, false, false, None)?;
                Ok(json!({"operation_ids":ids,"mode":"plan","reconcile":true}))
            }
            "observe" => {
                let mut c = controller().lock().unwrap();
                let id = c.observe_selected(v["target"].as_str().unwrap_or(""))?;
                Ok(json!({"operation_ids":[id],"mode":"observe"}))
            }
            "recover" => {
                let target = v["target"].as_str().unwrap_or("all");
                let mut c = controller().lock().unwrap();
                let ids = c.recover_explicit(target)?;
                Ok(json!({"operation_ids":ids,"mode":"recover"}))
            }
            "recover_reboot" => {
                let mut c = controller().lock().unwrap();
                let id = c.reboot_for_recovery(v["target"].as_str().unwrap_or(""))?;
                Ok(json!({"operation_ids":[id],"mode":"recover_reboot"}))
            }
            "retry" => {
                let target = v["target"].as_str().unwrap_or("all");
                let mut c = controller().lock().unwrap();
                let ids = c.start(target, true, false)?;
                let devices: Vec<_> = c
                    .operations
                    .iter()
                    .filter(|(_, op)| ids.contains(&op.id))
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in devices {
                    c.attempts.remove(&id);
                    c.next_retry_at.remove(&id);
                    c.read_recovery.remove(&id);
                }
                Ok(json!({"operation_ids":ids,"mode":"apply","retry":true}))
            }
            _ => Err("unknown provisioning operation".into()),
        }
    })()
    .unwrap_or_else(|e| error(&e));
    if changed {
        wake();
    }
    reply.to_string()
}

pub fn start(engine: Arc<Mutex<crate::pon_engine::PonEngine>>) {
    crate::omci_auth::install();
    {
        let mut c = controller().lock().unwrap();
        c.resources = Some(engine.lock().unwrap().resources);
        match config::load() {
            Ok(cfg) => {
                {
                    let mut engine = engine.lock().unwrap();
                    engine.onu_pins = cfg.onu_pins.clone();
                    if cfg
                        .onu_pins
                        .values()
                        .any(|id| !engine.resources.onu_ids.contains(id))
                    {
                        let error =
                            "configured ONU-ID is unavailable on the selected platform".to_owned();
                        engine.pin_error = Some(error.clone());
                        c.config_error = Some(error.clone());
                        c.failure = Some(error);
                    }
                }
                if let Err(e) = c.replace_config(cfg) {
                    engine.lock().unwrap().pin_error = Some(e.clone());
                    c.config_error = Some(e.clone());
                    c.failure = Some(e);
                }
            }
            Err(e) => {
                engine.lock().unwrap().pin_error = Some(e.clone());
                c.config_error = Some(e.clone());
                c.failure = Some(e);
            }
        }
        c.load_reset_intents();
        c.load_snapshots();
        c.retain_configured_state();
        c.resume_resets();
        c.startup_pending = c
            .config
            .devices
            .iter()
            .map(|device| device.id.clone())
            .collect();
        let identities: Vec<_> = c
            .config
            .devices
            .iter()
            .filter(|d| !c.resets.blocks(&d.optical_sn))
            .map(|d| d.cwmp_id.clone())
            .collect();
        for id in identities {
            match wan::load(&id) {
                Ok(Some(owned)) => {
                    c.owned.insert(id.clone(), owned);
                }
                Ok(None) => (),
                Err(e) => c.failure = Some(e),
            }
            match wifi::load(&id) {
                Ok(owned) => {
                    c.wifi_owned.insert(id, owned);
                }
                Err(e) => c.failure = Some(e),
            }
        }
    }
    crate::runtime::spawn_service("provisioning", async move {
        wake();
        while !crate::runtime::is_stopping() {
            let engine = engine.clone();
            let batch = std::mem::take(&mut *ready().lock().unwrap());
            let delay =
                match crate::runtime::spawn_blocking(move || scheduler::tick(&engine, batch)).await
                {
                    Ok(delay) => delay,
                    Err(error) => {
                        eprintln!("[provisioning] scheduler failed: {error}");
                        break;
                    }
                };
            tokio::select! {
                _ = WAKE.notified() => (),
                _ = tokio::time::sleep(delay) => wake(),
                _ = crate::runtime::cancelled() => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    fn discovered_identity(devices: &serde_json::Value, optical_sn: &str) -> Option<String> {
        super::discovered_identity(
            &crate::cwmp::observations::from_fixture(devices),
            optical_sn,
        )
        .map(str::to_owned)
    }
    use super::*;
    #[test]
    fn device_reset_forgets_live_and_recovered_state_only_for_target() {
        let op = operation(OperationPhase::Writing, false);
        let device = op.device.clone();
        let mut c = Controller::default();
        c.config.devices.push(device.clone());
        c.operations.insert(device.id.clone(), op);
        c.recovered
            .insert(device.id.clone(), json!({"state":"interrupted"}));
        c.recovered.insert(
            "other".into(),
            json!({"device":"other","state":"completed"}),
        );
        c.owned.insert(device.cwmp_id.clone(), BTreeMap::new());
        c.wifi_owned
            .insert(device.cwmp_id.clone(), wifi::Registry::default());
        c.wifi_owned
            .insert("other".into(), wifi::Registry::default());
        c.forget_device(&device.optical_sn, &device.cwmp_id);
        assert!(c.operations.is_empty());
        assert!(!c.recovered.contains_key(&device.id));
        assert!(c.recovered.contains_key("other"));
        assert!(!c.owned.contains_key(&device.cwmp_id));
        assert!(!c.wifi_owned.contains_key(&device.cwmp_id));
        assert!(c.wifi_owned.contains_key("other"));
        let snapshot = c.snapshot();
        assert_eq!(
            snapshot["devices"][0]["provisioning"]["state"],
            "not_started"
        );
        assert!(snapshot["devices"][0]["provisioning"]["operation_id"].is_null());
        let history: Value = serde_json::from_str(&c.history()).unwrap();
        assert_eq!(history["operations"].as_array().unwrap().len(), 1);
        assert_eq!(history["operations"][0]["device"], "other");
        assert!(c.reset_device("all").is_err());
    }
    #[test]
    fn sg631_identity_requires_feature_vendor_model_full_serial_and_unique_match() {
        let devices = json!({"devices":{
            "child":{"product_class":"SG631Z-C","serial":"MODEL-STAR1234ABCD-SUFFIX"}
        }});
        assert_eq!(
            discovered_identity(&devices, "STAR1234ABCD"),
            cfg!(feature = "device-star").then(|| "child".into())
        );
        for serial in ["STAR1234ABCE", "STAR1234ABC0", "HWTC1234ABCD"] {
            assert_eq!(discovered_identity(&devices, serial), None);
        }
        let wrong_model = json!({"devices":{
            "child":{"product_class":"OTHER","serial":"MODEL-STAR1234ABCD-SUFFIX"}
        }});
        assert_eq!(discovered_identity(&wrong_model, "STAR1234ABCD"), None);
        let ambiguous = json!({"devices":{
            "one":{"product_class":"SG631Z-C","serial":"MODEL-STAR1234ABCD-SUFFIX"},
            "two":{"product_class":"SG631Z-C","serial":"OTHER-STAR1234ABCD-SUFFIX"}
        }});
        assert_eq!(discovered_identity(&ambiguous, "STAR1234ABCD"), None);
    }
    #[test]
    fn business_status_uses_its_own_channel_and_disabled_devices_are_not_pending() {
        let mut c = Controller::default();
        let mut d = operation(OperationPhase::Completed, false).device;
        assert!(!c.business_ready(&d));
        crate::omci_auth::test_authorize(&d.optical_sn);
        c.management
            .insert(d.id.clone(), json!({"state":"waiting_inform"}));
        c.mqtt.insert(
            d.id.clone(),
            json!({"state":"online","optical_online":true}),
        );
        d.provisioning_adapter = "hl202-mqtt".into();
        assert!(c.business_ready(&d));
        d.provisioning_adapter = "hl202-cu".into();
        assert!(!c.business_ready(&d));
        c.management.get_mut(&d.id).unwrap()["state"] = json!("online");
        d.bootstrap_adapter = "none".into();
        assert!(c.business_ready(&d));
        d.provisioning_adapter = "none".into();
        d.bootstrap_adapter = "manual-acs".into();
        c.config.devices.push(d);
        let view = c.business_snapshot();
        assert_eq!(view["devices"][0]["provisioning"]["state"], "disabled");
        assert_eq!(view["devices"][0]["wifi_sync"], "disabled");
        assert_eq!(view["devices"][0]["management"]["state"], "online");
        assert_eq!(view["devices"][0]["mqtt"]["state"], "online");
        assert_eq!(view["pending_devices"], json!([]));
        assert_ne!(view["devices"][0]["acs_bootstrap"]["state"], "not_required");
    }
    #[test]
    fn connection_request_defaults_follow_observed_identity_and_preserve_explicit_credentials() {
        let mut c = Controller::default();
        let mut d = operation(OperationPhase::Completed, false).device;
        d.connection_request.defaults = true;
        assert!(c.connection_request_policy(&d).defaults);
        c.management
            .insert(d.id.clone(), json!({"product_class":"HL202-DU-C"}));
        for adapter in ["none", "hl202-mqtt", "hl202-cu", "vendor-ct-candidate"] {
            d.provisioning_adapter = adapter.into();
            let policy = c.connection_request_policy(&d);
            assert_eq!(policy.defaults, !cfg!(feature = "device-hl202"));
            if cfg!(feature = "device-hl202") {
                assert!(policy.credentials_available());
            }
        }
        d.connection_request.defaults = false;
        d.connection_request.username = "explicit-user".into();
        d.connection_request.password = "example-secret".into();
        assert_eq!(c.connection_request_policy(&d).username, "explicit-user");
        assert_eq!(c.connection_request_policy(&d).password, "example-secret");
    }
    #[test]
    fn hwtc_identity_matches_serial_across_models() {
        for model in ["K155-11", "V152", "OTHER-HWTC"] {
            let devices =
                json!({"devices":{"child":{"product_class":model,"serial":"485754431234ABCD"}}});
            assert_eq!(
                discovered_identity(&devices, "HWTC1234ABCD"),
                cfg!(feature = "device-hwtc").then(|| "child".into())
            );
            assert_eq!(discovered_identity(&devices, "HWTC9999ABCD"), None);
        }
    }
    #[test]
    fn hwtc_identity_rejects_other_vendors_and_ambiguous_serials() {
        let devices =
            json!({"devices":{"child":{"product_class":"V152","serial":"483343543C6DE5B3"}}});
        assert_eq!(discovered_identity(&devices, "H3CT1234ABCD"), None);
        let devices = json!({"devices":{
            "one":{"product_class":"V152","serial":"485754431234ABCD"},
            "two":{"product_class":"K155-11","serial":"485754431234ABCD"}
        }});
        assert_eq!(discovered_identity(&devices, "HWTC1234ABCD"), None);
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn bool_and_binding_equivalence() {
        let mut p = Parameter {
            name: "Enable".into(),
            value: "1".into(),
            kind: ParameterKind::Boolean,
            secret: false,
        };
        assert!(equivalent(&p, "true"));
        assert!(!equivalent(&p, "false"));
        p.name = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.WANIPConnection.1.X_CU_LanInterface".into();
        p.kind = ParameterKind::String;
        p.value = "a,b".into();
        assert!(equivalent(&p, "b,a"));
        let root = "InternetGatewayDevice.LANDevice.1.WLANConfiguration.";
        p.value = format!("{root}2,{root}6");
        assert!(equivalent(
            &p,
            &format!("{root}2,{root}6,{root}10,{root}14")
        ));
        assert!(!equivalent(&p, &format!("{root}2,{root}6,{root}11")));
    }
    pub(super) fn operation(stage: OperationPhase, secret: bool) -> Operation {
        let item = Item {
            wlan_intent: None,
            binding_intent: None,
            id: "wifi".into(),
            kind: ItemKind::Ssid,
            service_type: String::new(),
            guard: None,
            blocked: None,
            wan_ref: None,
            auto_instance: None,
            wan_intent: None,
            parameters: vec![Parameter {
                name: "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID".into(),
                value: "wanted".into(),
                kind: ParameterKind::String,
                secret,
            }],
        };
        let mut o = Operation::new(
            1,
            Device {
                id: "child".into(),
                optical_sn: "H3CT12345678".into(),
                cwmp_id: "id".into(),
                omci_adapter: "hl202".into(),
                bootstrap_adapter: "manual-acs".into(),
                provisioning_adapter: "hl202-cu".into(),
                resolved_contract: None,
                connection_request: Default::default(),
                items: vec![item],
                wifi_cleanup: "preserve".into(),
                wifi_preserve: BTreeSet::new(),
            },
            "rev".into(),
            true,
            false,
        );
        o.stage = stage;
        if stage == OperationPhase::Completed {
            o.complete(o.started);
        }
        o.task = Some(1);
        o
    }
    fn responses(v: Vec<Value>) {
        RPC.with(|r| *r.borrow_mut() = (v.into(), Vec::new()));
    }
    fn readback(value: &str) -> Value {
        json!({"state":"completed","result":{"parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID","value":value}]}})
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn unknown_write_is_read_before_any_retry() {
        let mut o = operation(OperationPhase::Writing, false);
        responses(vec![
            json!({"state":"unknown"}),
            json!({"id":2}),
            readback("old"),
        ]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Verifying);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "unknown");
        RPC.with(|r| assert_eq!(r.borrow().1[1]["method"], "GetParameterValues"));
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn status_one_requires_matching_readback() {
        let mut o = operation(OperationPhase::Writing, false);
        responses(vec![
            json!({"state":"accepted_pending_apply"}),
            json!({"id":2}),
            readback("old"),
        ]);
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "accepted_pending_apply");
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn credential_write_does_not_require_readback() {
        let mut o = operation(OperationPhase::Writing, true);
        let key = Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        };
        o.device.items[0].parameters = vec![key.clone()];
        o.changes = vec![key];
        responses(vec![
            json!({"state":"completed"}),
            json!({"id":2}),
            json!({"state":"completed","result":{"parameters":[]}}),
        ]);
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "verified");
        assert!(!o.view().to_string().contains("wanted"));
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn matching_readable_values_do_not_settle_unknown_or_pending_credentials() {
        for secret in [false, true] {
            for (state, expected) in [
                ("unknown", "unknown"),
                ("accepted_pending_apply", "accepted_pending_apply"),
                ("completed", "verified"),
            ] {
                let mut o = operation(OperationPhase::Writing, false);
                if secret {
                    let key = Parameter { name: "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase".into(), value: "test-only-key".into(), kind: ParameterKind::String, secret: true };
                    o.device.items[0].parameters.push(key.clone());
                    o.changes.push(key);
                } else {
                    o.changes = o.item().parameters.clone();
                }
                responses(vec![
                    json!({"state":state}),
                    json!({"id":2}),
                    readback("wanted"),
                ]);
                advance(&mut o, true);
                advance(&mut o, true);
                assert_eq!(
                    o.results["wifi"]["state"],
                    if secret { expected } else { "verified" },
                    "secret={secret}, write={state}"
                );
                if secret && state == "unknown" {
                    assert!(!o.results["wifi"]["reason"]
                        .as_str()
                        .unwrap()
                        .contains("was accepted"));
                }
            }
        }
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn hl202_passphrase_fault_retries_vendor_keypassphrase() {
        let mut o = operation(OperationPhase::Writing, true);
        let secret = Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        };
        o.device.items[0].parameters = vec![secret.clone()];
        o.changes = vec![secret];
        let fault = json!({"state":"fault","result":{"fault_code":"9003","parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase","fault_code":"9007","fault_string":"Invalid parameter value"}]}});
        let retried = json!({"id":9});
        let ok = json!({"state":"completed"});
        let read_ok = json!({"id":10});
        let readback = json!({"state":"completed","result":{"parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase","value":""},{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.KeyPassphrase","value":"secret12"}]}});
        responses(vec![fault, retried, ok, read_ok, readback]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Writing);
        assert_eq!(o.task, Some(9));
        RPC.with(|r| {
            let b = r.borrow();
            let q = b.1.last().unwrap();
            assert_eq!(q["method"], "SetParameterValues");
            assert_eq!(
                q["args"]["parameters"][0]["name"],
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.KeyPassphrase"
            );
        });
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "verified");
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn hwtc_passphrase_fault_does_not_retry() {
        let mut o = operation(OperationPhase::Writing, true);
        o.device.omci_adapter = "hwtc".into();
        o.device.provisioning_adapter = "hwtc-hw".into();
        o.changes = vec![Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        }];
        let fault = json!({"state":"fault","result":{"fault_code":"9003","parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase","fault_code":"9007"}]}});
        responses(vec![fault]);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "failed");
    }
    #[test]
    fn management_wan_is_never_converted_to_business_bridge() {
        for service in ["TR069,INTERNET", "TR069_INTERNET", "internet_tr069"] {
            for apply in [false, true] {
                let mut o = operation(OperationPhase::Reading, apply);
                o.device.omci_adapter = "hwtc".into();
                o.device.provisioning_adapter = "hwtc-hw".into();
                o.device.items[0].guard = Some("guard".into());
                o.device.items[0].service_type = "internet".into();
                let mut reply = readback("old");
                reply["result"]["parameters"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name":"guard","value":service}));
                responses(vec![reply]);
                advance(&mut o, true);
                assert_eq!(
                    o.results["wifi"]["state"], "blocked",
                    "{service}, apply={apply}"
                );
            }
        }
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn wan_changes_disable_first_and_activation_requires_bindings() {
        let mut base = operation(OperationPhase::Reading, false);
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4.WANIPConnection.1.";
        let service = Item {
            wlan_intent: None,
            binding_intent: None,
            id: "wan".into(),
            kind: ItemKind::DeviceService,
            service_type: "internet".into(),
            guard: None,
            blocked: None,
            wan_ref: Some("wan".into()),
            auto_instance: None,
            wan_intent: None,
            parameters: vec![
                Parameter {
                    name: format!("{root}Enable"),
                    value: "1".into(),
                    kind: ParameterKind::Boolean,
                    secret: false,
                },
                Parameter {
                    name: format!("{root}ConnectionType"),
                    value: "IP_Bridged".into(),
                    kind: ParameterKind::String,
                    secret: false,
                },
            ],
        };
        let mut binding = service.clone();
        binding.id = "binding".into();
        binding.kind = ItemKind::PortBinding;
        binding.parameters = vec![Parameter {
            name: format!("{root}X_CU_LanInterface"),
            value: "wanted".into(),
            kind: ParameterKind::String,
            secret: false,
        }];
        base.device.items = vec![service, binding];
        let mut o = Operation::new(1, base.device, "rev".into(), true, false);
        o.stage = OperationPhase::Reading;
        o.task = Some(1);
        assert_eq!(o.device.items.last().unwrap().kind, ItemKind::WanActivation);
        responses(vec![
            json!({"state":"completed","result":{"parameters":[{"name":format!("{root}Enable"),"value":"1"},{"name":format!("{root}ConnectionType"),"value":"IP_Routed"}]}}),
            json!({"id":2}),
        ]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Disabling);
        RPC.with(|r| {
            let b = r.borrow();
            let parameters = b.1.last().unwrap()["args"]["parameters"]
                .as_array()
                .unwrap();
            assert_eq!(parameters.len(), 1);
            assert_eq!(parameters[0]["name"], format!("{root}Enable"));
            assert_eq!(parameters[0]["value"], "0");
        });
        o.index = 2;
        o.stage = OperationPhase::Waiting;
        o.task = None;
        o.results
            .insert("wan".into(), json!({"state":"verified","revision":"rev"}));
        o.results
            .insert("binding".into(), json!({"state":"failed","revision":"rev"}));
        responses(vec![json!({"busy":false})]);
        advance(&mut o, true);
        assert_eq!(o.results["wan__activation"]["state"], "blocked");
        assert_eq!(o.results["wan"]["state"], "blocked");
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn activation_does_not_replace_original_wan_failure() {
        let mut o = operation(OperationPhase::Waiting, false);
        o.device.items[0].kind = ItemKind::WanActivation;
        o.device.items[0].wan_ref = Some("wan".into());
        for state in ["failed", "blocked", "unknown"] {
            o.index = 0;
            let original = json!({"state":state,"reason":"WAN write requires a fresh non-TR069 service readback","task_id":7898,"revision":"rev"});
            o.results.insert("wan".into(), original.clone());
            o.finish_item(
                ItemOutcome::Blocked,
                "verify the referenced WAN service first",
            );
            assert_eq!(o.results["wan"], original);
        }
    }
    #[test]
    #[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 contract")]
    fn matching_active_wan_is_not_disabled_on_every_sync() {
        let mut o = operation(OperationPhase::Reading, false);
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4.WANIPConnection.1.";
        let item = &mut o.device.items[0];
        item.id = "wan".into();
        item.kind = ItemKind::DeviceService;
        item.wan_ref = Some("wan".into());
        item.parameters = vec![
            Parameter {
                name: format!("{root}Enable"),
                value: "1".into(),
                kind: ParameterKind::Boolean,
                secret: false,
            },
            Parameter {
                name: format!("{root}ConnectionType"),
                value: "IP_Bridged".into(),
                kind: ParameterKind::String,
                secret: false,
            },
        ];
        let mut o = Operation::new(1, o.device, "rev".into(), true, false);
        o.stage = OperationPhase::Reading;
        o.task = Some(1);
        responses(vec![
            json!({"state":"completed","result":{"parameters":[{"name":format!("{root}Enable"),"value":"1"},{"name":format!("{root}ConnectionType"),"value":"IP_Bridged"}]}}),
        ]);
        advance(&mut o, true);
        assert_eq!(o.results["wan"]["state"], "verified");
        RPC.with(|r| {
            assert!(r
                .borrow()
                .1
                .iter()
                .all(|v| v["method"] != "SetParameterValues"))
        });
    }

    #[test]
    fn blocked_capabilities_populate_service_type_on_all_items() {
        let mut op = operation(OperationPhase::Discovering, false);
        op.device.items[0].service_type = "wifi".into();
        op.capabilities.invalidate("test contract fault");
        assert!(op.capabilities.blocked());
        let reason = op.capabilities.view()["reason"]
            .as_str()
            .unwrap_or("capability contract unresolved")
            .to_owned();
        for item in &op.device.items {
            op.results.entry(item.id.clone()).or_insert_with(|| {
                json!({
                    "id": item.id,
                    "kind": item.kind,
                    "service_type": item.service_type,
                    "state": "blocked",
                    "reason": reason,
                    "revision": op.revision,
                })
            });
        }
        let result = &op.results["wifi"];
        assert_eq!(result["state"], "blocked");
        assert_eq!(result["service_type"], "wifi");

        let mut c = Controller::default();
        c.config.devices = vec![op.device.clone()];
        c.operations.insert("child".into(), op);
        let snapshot = c.snapshot();
        let items = snapshot["devices"][0]["provisioning"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(items[0]["service_type"], "wifi");
        assert_eq!(items[0]["state"], "blocked");
    }

    #[test]
    fn plan_mode_marks_secret_only_difference_as_accepted_unverified() {
        let mut o = operation(OperationPhase::Reading, false);
        o.apply = false;
        let secret = Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        };
        o.device.items[0].parameters = vec![secret];
        let readback = json!({
            "state": "completed",
            "result": {
                "parameters": [{
                    "name": "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase",
                    "value": ""
                }]
            }
        });
        responses(vec![readback]);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "accepted_unverified");
        assert!(o.results["wifi"]["reason"]
            .as_str()
            .unwrap()
            .contains("write-only credentials"));
    }

    #[test]
    fn production_snapshots_match_shared_contract_before_status_composition() {
        let mut controller = Controller::default();
        for phase in [
            None,
            Some(OperationPhase::Waiting),
            Some(OperationPhase::Completed),
        ] {
            controller.operations.clear();
            let operation = operation(phase.unwrap_or(OperationPhase::Waiting), false);
            controller.config.devices = vec![operation.device.clone()];
            if phase.is_some() {
                controller.operations.insert("child".into(), operation);
            }
            let value = controller.snapshot();
            let typed: fttr_api::SyncStatusReply = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(typed.devices.len(), 1);
            assert_eq!(typed.devices[0].device, "child");
            assert_eq!(typed.devices[0].ssids[0].id, "wifi");
            assert_eq!(
                serde_json::to_value(&typed.devices[0].provisioning.state).unwrap(),
                value["devices"][0]["provisioning"]["state"]
            );
            assert_eq!(
                serde_json::to_value(typed.devices[0].wifi_sync).unwrap(),
                value["devices"][0]["wifi_sync"]
            );
        }
    }

    #[test]
    fn snapshot_reports_self_healing_attempts_and_waiting_management() {
        let mut c = Controller::default();
        let op = operation(OperationPhase::Waiting, false);
        c.config.devices = vec![op.device.clone()];
        c.operations.insert("child".into(), op);
        c.attempts.insert("child".into(), 2);
        c.next_retry_at.insert("child".into(), 12345678);
        c.management
            .insert("child".into(), json!({"state":"offline_or_stale"}));

        let snap = c.snapshot();
        let dev = &snap["devices"][0];
        assert_eq!(dev["provisioning"]["state"], "waiting_management");
        assert_eq!(dev["provisioning"]["attempts"], 2);
        assert_eq!(dev["provisioning"]["next_retry_at"], 12345678);
        assert_eq!(dev["wifi_sync"], "waiting_management");
    }

    #[test]
    fn snapshot_reports_recovering_when_reconcile_required() {
        let mut c = Controller::default();
        let mut op = operation(OperationPhase::Completed, false);
        op.results.insert(
            "wifi".into(),
            json!({
                "id": "wifi",
                "kind": "ssid",
                "service_type": "wifi",
                "state": "reconcile_required",
                "reason": "service_restarted",
                "revision": op.revision,
            }),
        );
        c.config.devices = vec![op.device.clone()];
        c.operations.insert("child".into(), op);
        c.management
            .insert("child".into(), json!({"state":"online"}));

        let snap = c.snapshot();
        let dev = &snap["devices"][0];
        assert_eq!(dev["wifi_sync"], "recovering");
    }
}
