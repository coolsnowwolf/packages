//! Per-device ACS bootstrap; MQTT, settings verification and Inform are independent evidence.
use crate::mqtt_identity::Registry;
use crate::provisioning::{
    management_network,
    model::{Device, Management},
    storage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn state_path() -> String {
    crate::provisioning::runtime_store::Record::Bootstrap.path("")
}
const MAX_ATTEMPTS: u32 = 5;

use fttr_api::BootstrapPhase as Phase;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Record {
    optical_sn: String,
    cwmp_id: String,
    mqtt_target: Option<String>,
    binding_source: String,
    method: String,
    desired_version: String,
    generation: u64,
    state: Phase,
    reason: String,
    attempts: u32,
    // A durable write intent is not remote cancellation or terminal evidence.
    // Preserve it across invalidation/restart and never automatically replay it.
    write_pending: Option<u64>,
    next_retry_at: u64,
    updated_at: u64,
    settings_verified_at: Option<u64>,
    selected_vlan: Option<u32>,
    last_inform: Option<u64>,
    expected_acs_url: String,
    last_error: Option<String>,
    transport_epoch: u64,
    completed_authorization: u64,
    connection_request_enabled: Option<bool>,
    #[serde(skip)]
    worker: Option<u64>,
    #[serde(skip)]
    initial_inform: Option<Option<u64>>,
    #[serde(skip)]
    optical_online: bool,
    #[serde(skip)]
    require_inform_after: u64,
    #[serde(skip)]
    last_observed_time: u64,
    #[serde(skip)]
    removed: bool,
}

#[derive(Default)]
struct Coordinator {
    records: BTreeMap<String, Record>,
    saved: Vec<u8>,
    persistence_error: Option<String>,
    load_blocked: bool,
    tasks: BTreeMap<(String, u64), Worker>,
}

struct Worker(tokio::task::JoinHandle<Result<bool, String>>);
impl Worker {
    fn start(
        work: impl std::future::Future<Output = Result<bool, String>> + Send + 'static,
        deadline: std::time::Duration,
    ) -> Self {
        Self(crate::runtime::spawn(async move {
            struct Wake;
            impl Drop for Wake {
                fn drop(&mut self) {
                    crate::provisioning::controller::wake();
                }
            }
            let _wake = Wake;
            tokio::time::timeout(deadline, work)
                .await
                .unwrap_or_else(|_| {
                    Err(
                        "bootstrap effect deadline exceeded; remote execution is not cancelled"
                            .into(),
                    )
                })
        }))
    }
    fn take(&mut self) -> Option<Result<bool, String>> {
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        match Pin::new(&mut self.0).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Pending => None,
            Poll::Ready(result) => Some(result.unwrap_or_else(|_| {
                Err("bootstrap executor terminated; execution outcome not established".into())
            })),
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// Harvest even after removal, authorization loss or loss of both transports.
// Completions always route to their original generation, never a new grant.
pub(super) fn collect() {
    let mut c = coordinator().lock().unwrap();
    if c.collect_workers(now()) {
        c.persist();
    }
}

pub(crate) fn external_command_allowed(serial: &str) -> bool {
    let c = coordinator().lock().unwrap();
    !c.load_blocked
        && c.persistence_error.is_none()
        && !c
            .records
            .values()
            .any(|r| r.optical_sn == serial && (r.write_pending.is_some() || r.worker.is_some()))
}

static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();
fn coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| {
        let mut c = Coordinator::default();
        let bytes = storage::read(state_path());
        match bytes {
            Ok(bytes) => match decode_records(&bytes) {
                Ok(records) => c.records = records,
                Err(e) => {
                    c.load_blocked = true;
                    c.persistence_error = Some(format!("bootstrap state decode: {e}"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => {
                c.load_blocked = true;
                c.persistence_error = Some(format!("bootstrap state read: {e}"));
            }
        }
        c.restart(now());
        Mutex::new(c)
    })
}

fn decode_records(bytes: &[u8]) -> Result<BTreeMap<String, Record>, serde_json::Error> {
    let fields: BTreeMap<String, Value> = serde_json::from_slice(bytes)?;
    let mut records: BTreeMap<String, Record> = serde_json::from_slice(bytes)?;
    for (id, record) in &mut records {
        // Preserve uncertainty when importing a pre-intent journal. Its generic
        // error cannot distinguish an initial-read failure from a lost write reply.
        if fields[id].get("write_pending").is_none()
            && record.method == "hl202-mqtt-acs"
            && record.attempts != 0
            && record.last_error.is_some()
        {
            record.write_pending = Some(record.generation);
        }
    }
    Ok(records)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(feature = "mqtt")]
pub(crate) fn observe_mqtt(topic: &str, time: u64) {
    if let Ok(mut registry) = crate::mqtt_identity::shared().lock() {
        registry.observe(topic, time);
    }
}

fn retry_delay(attempts: u32) -> u64 {
    (5u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(6))).min(300)
}

fn version(device: &Device, management: Option<&Management>, target: Option<&str>) -> String {
    let bytes = serde_json::to_vec(&(
        device.optical_sn.as_str(),
        device.bootstrap_adapter.as_str(),
        device.connection_request.enabled,
        management,
        target,
    ))
    .unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

impl Record {
    fn invalidate(&mut self, time: u64, reason: &str) {
        self.generation = self.generation.saturating_add(1);
        self.settings_verified_at = None;
        self.selected_vlan = None;
        self.require_inform_after = time;
        self.next_retry_at = 0;
        self.last_error = None;
        self.set_phase(Phase::WaitingTransport, time, reason);
    }
    fn set_phase(&mut self, state: Phase, time: u64, reason: &str) {
        if self.state != state || self.reason != reason {
            self.state = state;
            self.reason = reason.into();
            self.updated_at = time;
        }
    }
    fn fresh_inform(&self, at: Option<u64>, time: u64, freshness: u64, online: bool) -> bool {
        online
            && at.is_some_and(|at| {
                at <= time
                    && time.saturating_sub(at) <= freshness
                    && at >= self.require_inform_after
                    && self
                        .initial_inform
                        .is_some_and(|initial| initial != Some(at))
            })
    }
    fn finish(&mut self, generation: u64, result: Result<bool, String>, time: u64) {
        if self.worker == Some(generation) {
            self.worker = None;
        }
        if self.generation != generation || self.state == Phase::Ready {
            return;
        }
        match result {
            Ok(true) => {
                if self.write_pending == Some(generation) {
                    self.write_pending = None;
                }
                self.settings_verified_at = Some(time);
                self.last_error = None;
                self.next_retry_at = time.saturating_add(60);
                self.set_phase(Phase::WaitingInform, time, "CWMP settings verified; waiting for a fresh Inform to confirm ACS reachability");
            }
            Ok(false) => (),
            Err(e) => {
                self.last_error = Some(e);
                self.next_retry_at = time.saturating_add(retry_delay(self.attempts));
                self.set_phase(Phase::RetryWait, time, "bootstrap attempt failed or has unknown outcome; retry will read current settings first");
            }
        }
    }
}

pub(super) struct Observation<'a> {
    pub device: &'a Device,
    pub management: Option<&'a Management>,
    pub last_inform: Option<u64>,
    pub online: bool,
    pub new_boot: bool,
    pub freshness: u64,
    pub time: u64,
    pub acs_available: bool,
    pub authorization: Option<(u64, bool)>,
}

struct Job {
    id: String,
    generation: u64,
    target: String,
    url: String,
    persist_matching: bool,
    vlans: Vec<u32>,
    priority: u32,
    authorization_epoch: u64,
}

pub(super) fn retain_devices(ids: &std::collections::BTreeSet<String>) {
    let mut c = coordinator().lock().unwrap();
    for (id, r) in &mut c.records {
        if !ids.contains(id) && !r.removed {
            r.invalidate(
                now(),
                "device removed from configuration; obsolete worker must exit",
            );
            r.removed = true;
        }
        if ids.contains(id) {
            r.removed = false;
        }
    }
    c.records
        .retain(|id, r| ids.contains(id) || r.worker.is_some() || r.write_pending.is_some());
    c.persist();
}

/// Cancel obsolete bootstrap workers as soon as a new configuration is activated.
pub(super) fn configure(devices: &[Device]) {
    let mut c = coordinator().lock().unwrap();
    c.configure(devices, now());
    c.persist();
}

/// Scheduler entry point. Remote commands execute outside controller and bootstrap locks.
pub(super) fn tick(observation: Observation<'_>) {
    let Some(auth_epoch) = crate::omci_auth::serial_epoch(&observation.device.optical_sn) else {
        return;
    };
    let Ok(sources) = crate::mqtt_identity::shared().lock() else {
        return;
    };
    let mut c = coordinator().lock().unwrap();
    let job = c.schedule(observation, &sources);
    drop(sources);
    let job = c.durable_job(job, |bytes| {
        storage::write_atomic(&state_path(), bytes, true)
    });
    if let Some(job) = job {
        let key = (job.id.clone(), job.generation);
        let serial = c.records[&job.id].optical_sn.clone();
        c.tasks.insert(
            key,
            Worker::start(
                async move {
                    crate::omci_auth::execution_scope(
                        serial,
                        auth_epoch,
                        run(
                            job.id,
                            job.generation,
                            job.target,
                            job.url,
                            job.persist_matching,
                            job.vlans,
                            job.priority,
                            job.authorization_epoch,
                            auth_epoch,
                        ),
                    )
                    .await
                },
                std::time::Duration::from_secs(8 * 60),
            ),
        );
    }
}

impl Coordinator {
    fn collect_workers(&mut self, time: u64) -> bool {
        let terminal: Vec<_> = self
            .tasks
            .iter_mut()
            .filter_map(|(key, worker)| worker.take().map(|result| (key.clone(), result)))
            .collect();
        let changed = !terminal.is_empty();
        for ((id, generation), result) in terminal {
            self.tasks.remove(&(id.clone(), generation));
            if let Some(record) = self.records.get_mut(&id) {
                record.finish(generation, result, time);
            }
        }
        changed
    }
    fn configure(&mut self, devices: &[Device], time: u64) {
        for (id, r) in &mut self.records {
            let selected = devices
                .iter()
                .find(|d| &d.id == id && d.bootstrap_adapter != "none");
            if selected.is_none_or(|d| {
                d.optical_sn != r.optical_sn
                    || d.bootstrap_adapter != r.method
                    || Some(d.connection_request.enabled) != r.connection_request_enabled
            }) {
                r.invalidate(
                    time,
                    "bootstrap selection or device changed; obsolete worker must exit",
                );
                r.removed = true;
            }
        }
        self.records
            .retain(|_, r| !r.removed || r.worker.is_some() || r.write_pending.is_some());
    }

    fn restart(&mut self, time: u64) {
        for record in self.records.values_mut() {
            // Older records may predate the explicit intent field. An interrupted
            // write/verification phase cannot be converted to a fresh write grant.
            if matches!(
                record.state,
                Phase::Configuring
                    | Phase::ConfiguringManagementWan
                    | Phase::Verifying
                    | Phase::VerifyingManagementWan
            ) {
                record.write_pending.get_or_insert(record.generation);
            }
            record.invalidate(
                time,
                "daemon restarted; fresh transport and Inform required",
            );
        }
    }

    fn schedule(&mut self, observation: Observation<'_>, sources: &Registry) -> Option<Job> {
        let capacity = !crate::runtime::is_stopping()
            && self.records.values().filter(|r| r.worker.is_some()).count() < 2;
        let Observation {
            device,
            management,
            last_inform: at,
            online,
            new_boot,
            freshness,
            time,
            acs_available,
            authorization,
        } = observation;
        let automatic = if device.bootstrap_adapter == "hl202-mqtt-acs" {
            sources.automatic_source(&device.optical_sn, time)
        } else {
            Ok(None)
        };
        let automatic_target = automatic
            .as_ref()
            .ok()
            .and_then(|source| source.map(|(mac, _)| mac));
        let binding_source = if automatic_target.is_some() {
            "onu_link"
        } else {
            "unresolved"
        };
        let target = automatic_target;
        let transport = automatic
            .as_ref()
            .ok()
            .and_then(|source| source.map(|(_, source)| source.epoch));
        let r = self.records.entry(device.id.clone()).or_default();
        if r.last_observed_time > time {
            r.invalidate(time, "system clock moved backwards; fresh Inform required");
            r.initial_inform = Some(at);
        }
        r.last_observed_time = time;
        let desired = format!(
            "{}-{}",
            version(device, management, target),
            authorization.map(|(epoch, _)| epoch).unwrap_or_default()
        );
        let initialized = !r.desired_version.is_empty();
        if r.connection_request_enabled
            .is_some_and(|enabled| enabled != device.connection_request.enabled)
        {
            r.completed_authorization = 0;
        }
        r.connection_request_enabled = Some(device.connection_request.enabled);
        if r.initial_inform.is_none() {
            r.initial_inform = Some(at);
        }
        if r.desired_version != desired {
            r.invalidate(
                time,
                "ACS bootstrap configuration or device identity changed",
            );
            r.desired_version = desired;
            r.initial_inform = Some(at);
        }
        r.optical_sn = device.optical_sn.clone();
        r.cwmp_id = device.cwmp_id.clone();
        r.mqtt_target = target.map(str::to_owned);
        r.binding_source = binding_source.into();
        r.method.clone_from(&device.bootstrap_adapter);
        if r.optical_online && !online {
            r.invalidate(
                time,
                "optical device offline; previous settings evidence invalidated",
            );
        }
        r.optical_online = online;
        if let Some(epoch) = transport {
            if r.transport_epoch != epoch {
                if r.transport_epoch != 0 {
                    r.invalidate(time, "MQTT transport reappeared; recheck device settings");
                }
                r.transport_epoch = epoch;
            }
        }
        if new_boot && initialized && at != r.last_inform {
            r.invalidate(
                at.unwrap_or(time),
                "device BOOT Inform invalidated previous settings evidence",
            );
        }
        r.last_inform = at;
        r.expected_acs_url = management.map(|m| m.acs_url.clone()).unwrap_or_default();
        // A recent Inform proves reachability, but does not prove that the
        // currently authorized ACS credentials have been written to the CPE.
        // Require a post-readback Inform before declaring the bootstrap ready.
        let settings_verified = device.bootstrap_adapter != "hl202-mqtt-acs"
            || r.settings_verified_at
                .is_some_and(|verified| at.is_some_and(|at| at >= verified));
        let fresh = acs_available
            && settings_verified
            && r.fresh_inform(
                at,
                time,
                device.connection_request.effective_freshness(freshness),
                online,
            );
        // Transport freshness is a separate observation, not a configuration change.
        // Keep readback evidence when only the Inform expires.
        if r.write_pending.is_some() && r.worker.is_none() {
            r.set_phase(Phase::AwaitingApply, time, "unresolved bootstrap write; terminal execution evidence and transaction-aware recovery required");
        } else if fresh && r.write_pending.is_none() {
            if let Some((epoch, _)) = authorization {
                r.completed_authorization = epoch;
            }
            r.set_phase(
                Phase::Ready,
                time,
                "fresh CWMP Inform received; management WAN bootstrap is unnecessary",
            );
            r.next_retry_at = 0;
            r.last_error = None;
            r.attempts = 0;
        } else if r.worker.is_some() {
            // Keep the slot until an obsolete worker exits; never overlap per-device commands.
        } else if management.is_none() {
            r.set_phase(
                Phase::NotConfigured,
                time,
                "management ACS URL is not configured",
            );
        } else if !acs_available {
            r.set_phase(
                Phase::WaitingAcs,
                time,
                "local ACS is disabled, unavailable or has a listener error",
            );
        } else if device.bootstrap_adapter == "manual-acs" {
            r.set_phase(
                Phase::ManualRequired,
                time,
                "configure the selected manual ACS path and wait for a fresh Inform",
            );
        } else if r.settings_verified_at.is_some() && r.completed_authorization != 0 {
            r.set_phase(
                Phase::WaitingInform,
                time,
                "settings remain verified; waiting for current CWMP reachability evidence",
            );
        } else if authorization.is_none_or(|(epoch, automatic)| {
            !automatic && (epoch == 0 || epoch == r.completed_authorization)
        }) {
            r.set_phase(Phase::AwaitingApply,time,"management drift or reset detected; apply management configuration to authorize recovery");
        } else if device.bootstrap_adapter == "omci-acs" {
            r.set_phase(Phase::WaitingInform, time, "waiting for OMCI ACS association and a fresh Inform; association readback alone does not prove reachability");
        } else if device.bootstrap_adapter != "hl202-mqtt-acs" {
            r.set_phase(
                Phase::ManualRequired,
                time,
                "configure the selected manual ACS path and wait for a fresh Inform",
            );
        } else if target.is_none() {
            r.set_phase(Phase::WaitingIdentity, time, automatic.err().unwrap_or("waiting for a unique fresh MQTT source MAC learned on this device's online ONU interface"));
        } else if !online {
            r.set_phase(
                Phase::WaitingTransport,
                time,
                "waiting for optical online and fresh MQTT telemetry from the associated device",
            );
        } else if let (Some(target), Some(management)) = (target, management) {
            if r.attempts >= MAX_ATTEMPTS {
                r.set_phase(
                    Phase::AwaitingApply,
                    time,
                    "bootstrap attempt budget exhausted; explicit recovery required",
                );
                return None;
            }
            if capacity && time >= r.next_retry_at {
                r.attempts = r.attempts.saturating_add(1);
                r.worker = Some(r.generation);
                r.set_phase(
                    Phase::Reading,
                    time,
                    "reading current CWMP settings before any conditional write",
                );
                return Some(Job {
                    id: device.id.clone(),
                    generation: r.generation,
                    target: target.to_owned(),
                    url: management.acs_url.clone(),
                    persist_matching: r.settings_verified_at.is_none() || r.last_error.is_some(),
                    vlans: management.vlans.clone(),
                    priority: management.priority,
                    authorization_epoch: authorization.map(|(epoch, _)| epoch).unwrap_or_default(),
                });
            }
            if r.state != Phase::WaitingInform {
                r.set_phase(
                    Phase::RetryWait,
                    time,
                    "retry scheduled; the next attempt starts with settings readback",
                );
            }
        }
        None
    }
}

impl Coordinator {
    fn persist(&mut self) -> bool {
        self.persist_with(|bytes| storage::write_atomic(&state_path(), bytes, true))
    }

    fn persist_with(&mut self, write: impl FnOnce(&[u8]) -> std::io::Result<()>) -> bool {
        if self.load_blocked {
            return false;
        }
        #[derive(Serialize)]
        struct Intent<'a> {
            optical_sn: &'a str,
            cwmp_id: &'a str,
            method: &'a str,
            generation: u64,
            write_pending: u64,
            state: Phase,
        }
        let restart: BTreeMap<_, _> = self
            .records
            .iter()
            .filter_map(|(id, record)| {
                record.write_pending.map(|pending| {
                    (
                        id,
                        Intent {
                            optical_sn: &record.optical_sn,
                            cwmp_id: &record.cwmp_id,
                            method: &record.method,
                            generation: record.generation,
                            write_pending: pending,
                            state: Phase::AwaitingApply,
                        },
                    )
                })
            })
            .collect();
        let bytes = match serde_json::to_vec(&restart) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.persistence_error = Some(format!("bootstrap state encode: {error}"));
                return false;
            }
        };
        if bytes != self.saved {
            match write(&bytes) {
                Ok(()) => {
                    self.saved = bytes;
                    self.persistence_error = None;
                }
                Err(e) => {
                    // A failed rename/directory barrier may already have changed
                    // the visible file. Even reverting to the old bytes must fsync.
                    self.saved.clear();
                    self.persistence_error = Some(format!("bootstrap state persistence: {e}"));
                    return false;
                }
            }
        }
        true
    }

    fn durable_job(
        &mut self,
        job: Option<Job>,
        write: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> Option<Job> {
        if self.persist_with(write) {
            return job;
        }
        if let Some(job) = &job {
            if let Some(record) = self.records.get_mut(&job.id) {
                if record.worker == Some(job.generation) {
                    record.worker = None;
                    record.next_retry_at = record
                        .updated_at
                        .saturating_add(retry_delay(record.attempts));
                    record.set_phase(
                        Phase::RetryWait,
                        record.updated_at,
                        "bootstrap persistence barrier failed; no worker dispatched",
                    );
                }
            }
        }
        None
    }

    fn advance(
        &mut self,
        id: &str,
        generation: u64,
        state: Phase,
        time: u64,
        reason: &str,
        write: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> bool {
        let Some(record) = self.records.get_mut(id) else {
            return false;
        };
        if record.generation != generation
            || record.worker != Some(generation)
            || record.state == Phase::Ready
        {
            return false;
        }
        if matches!(state, Phase::Configuring | Phase::ConfiguringManagementWan) {
            if record.write_pending.is_some_and(|old| old != generation) {
                return false;
            }
            record.write_pending = Some(generation);
            record.settings_verified_at = None;
        }
        record.set_phase(state, time, reason);
        self.persist_with(write)
    }
}

fn progress(id: &str, generation: u64, auth_epoch: u64, state: Phase, reason: &str) -> bool {
    let mut c = coordinator().lock().unwrap();
    let Some(r) = c.records.get_mut(id) else {
        return false;
    };
    if crate::omci_auth::serial_epoch(&r.optical_sn) != Some(auth_epoch) {
        return false;
    }
    if matches!(state, Phase::Configuring | Phase::ConfiguringManagementWan)
        && !crate::provisioning::runtime_store::approved(&r.optical_sn)
    {
        return false;
    }
    c.advance(id, generation, state, now(), reason, |bytes| {
        storage::write_atomic(&state_path(), bytes, true)
    })
}

async fn progress_async(
    id: &str,
    generation: u64,
    auth_epoch: u64,
    state: Phase,
    reason: &'static str,
) -> bool {
    let id = id.to_owned();
    crate::runtime::spawn_blocking(move || progress(&id, generation, auth_epoch, state, reason))
        .await
        .unwrap_or(false)
}

async fn run(
    id: String,
    generation: u64,
    target: String,
    url: String,
    persist_matching: bool,
    vlans: Vec<u32>,
    priority: u32,
    authorization_epoch: u64,
    auth_epoch: u64,
) -> Result<bool, String> {
    async {
        let credentials = management_network::credentials(authorization_epoch)
            .ok_or("management authorization changed before execution")?;
        if !progress_async(
            &id,
            generation,
            auth_epoch,
            Phase::DiscoveringManagementWan,
            "discovering the dedicated TR069 management WAN",
        )
        .await
        {
            return Ok(false);
        }
        let selected = crate::devices::ensure_mqtt_management_wan_async(
            &target,
            &vlans,
            priority,
            persist_matching,
            |stage| {
                let id = id.clone();
                async move {
                    if management_network::credentials(authorization_epoch).is_none() {
                        return false;
                    }
                    match stage {
                        crate::devices::ManagementWanStage::Configuring => {
                            progress_async(
                                &id,
                                generation,
                                auth_epoch,
                                Phase::ConfiguringManagementWan,
                                "conditionally configuring and persisting the dedicated TR069 WAN",
                            )
                            .await
                        }
                        crate::devices::ManagementWanStage::Verifying => progress_async(
                            &id,
                            generation,
                            auth_epoch,
                            Phase::VerifyingManagementWan,
                            "reading back management VLAN, routing, DHCP and unbound interfaces",
                        )
                        .await,
                    }
                }
            },
        )
        .await?;
        let record_id = id.clone();
        if !crate::runtime::spawn_blocking(move || {
            let mut c = coordinator().lock().unwrap();
            let Some(r) = c.records.get_mut(&record_id) else {
                return false;
            };
            if r.generation != generation || r.worker != Some(generation) {
                return false;
            }
            r.selected_vlan = Some(selected);
            c.persist()
        })
        .await
        .map_err(|_| "bootstrap persistence worker failed")?
        {
            return Ok(false);
        }
        if !progress_async(
            &id,
            generation,
            auth_epoch,
            Phase::Reading,
            "reading CWMP configuration after management WAN verification",
        )
        .await
        {
            return Ok(false);
        }
        let matched =
            crate::devices::read_authenticated_cwmp_settings_async(&target, &url, &credentials)
                .await?;
        if !matched || persist_matching {
            if !progress_async(
                &id,
                generation,
                auth_epoch,
                Phase::Configuring,
                "conditionally correcting and persisting CWMP settings",
            )
            .await
            {
                return Ok(false);
            }
            if management_network::credentials(authorization_epoch).is_none() {
                return Ok(false);
            }
            crate::devices::configure_authenticated_cwmp_settings_async(
                &target,
                &url,
                &credentials,
            )
            .await?;
        }
        if !progress_async(
            &id,
            generation,
            auth_epoch,
            Phase::Verifying,
            "independently reading back CWMP settings",
        )
        .await
        {
            return Ok(false);
        }
        if !crate::devices::read_authenticated_cwmp_settings_async(&target, &url, &credentials)
            .await?
        {
            return Err("CWMP settings do not match after configuration".into());
        }
        Ok(true)
    }
    .await
}

fn public_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}/{path}")
        }
        None => url.into(),
    }
}

pub(super) fn is_ready_for_management(id: &str, sn: &str, url: &str, epoch: u64) -> bool {
    let c = coordinator().lock().unwrap();
    c.persistence_error.is_none()
        && !c.load_blocked
        && c.records.get(id).is_some_and(|r| {
            r.state == Phase::Ready
                && r.write_pending.is_none()
                && r.optical_sn == sn
                && r.expected_acs_url == url
                && r.completed_authorization == epoch
        })
}
pub(super) fn omci_write_allowed(id: &str, epoch: u64, automatic: bool) -> bool {
    let c = coordinator().lock().unwrap();
    !c.load_blocked
        && c.persistence_error.is_none()
        && c.records.get(id).is_none_or(|r| {
            r.write_pending.is_none()
                && r.state != Phase::Ready
                && (automatic || (epoch != 0 && r.completed_authorization != epoch))
        })
}

pub(super) fn view(
    device: &Device,
    management: Option<&Management>,
    acs: &crate::cwmp::observations::ListenerObservation,
) -> Value {
    if device.bootstrap_adapter == "none" {
        return json!({"state":"not_required","reason":"management bootstrap is disabled for this device"});
    }
    let sources = if device.bootstrap_adapter == "hl202-mqtt-acs" {
        match crate::mqtt_identity::shared().lock() {
            Ok(sources) => Some(sources),
            Err(_) => {
                return json!({"state":"unavailable","reason":"MQTT identity state unavailable"})
            }
        }
    } else {
        None
    };
    let c = coordinator().lock().unwrap();
    let Some(r) = c.records.get(&device.id) else {
        return json!({"state":"pending","reason":"bootstrap observation not started"});
    };
    if r.removed || r.optical_sn != device.optical_sn || r.method != device.bootstrap_adapter {
        return json!({"state":"pending","reason":"waiting for observation of the selected bootstrap configuration"});
    }
    let mut view = serde_json::to_value(r).unwrap_or_default();
    view["inflight"] = json!(r.worker.is_some());
    view["persistence_error"] = json!(c.persistence_error);
    view["link_identity_error"] = json!(sources.as_ref().and_then(|s| s.topology_error()));
    view["listener"] = serde_json::to_value(fttr_api::AcsListener {
        enabled: acs.enabled,
        listen: acs.listen.clone(),
        path: acs.path.clone(),
        error: acs.error.clone(),
    })
    .expect("ACS listener serialization");
    view["mqtt_last_seen"] = json!(r
        .mqtt_target
        .as_ref()
        .and_then(|t| sources.as_ref().and_then(|s| s.last_seen(t))));
    if let Some(m) = management {
        let url = public_url(&m.acs_url);
        view["desired"] = json!({"vlan":r.selected_vlan.filter(|v|m.vlans.contains(v)).unwrap_or(m.vlan),"vlans":m.vlans,"priority":m.priority,"acs_url":url});
        if let (Some(listen), Some(path)) = (acs.listen.as_deref(), acs.path.as_deref()) {
            let expected = format!("http://{listen}{path}");
            if url != expected {
                view["endpoint_notice"] = json!(format!("Configured ACS URL {url} differs from the local listener {expected}; confirm routing or forwarding and wait for a fresh Inform."));
            }
        }
        view["manual_steps"] = if r.state == Phase::Ready {
            json!([])
        } else {
            json!([
            format!("If no existing management path reaches the ACS, configure TR069 VLAN {} with priority {}; retain a working management WAN.", m.vlan, m.priority),
            format!("Enable CWMP and periodic Inform; set ACS URL to {url}."),
            "Confirm a new Inform reaches this ACS; MQTT online alone is not CWMP verification."
        ])
        };
    }
    view
}

#[cfg(test)]
mod interval_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt_identity::normalize_target;

    #[tokio::test]
    async fn supervised_workers_settle_once_and_preserve_obsolete_write_intent() {
        use std::time::Duration;
        for mode in 0..4 {
            let mut c = active_record();
            c.records.get_mut("test-device").unwrap().write_pending = Some(7);
            let worker = Worker::start(
                async move {
                    match mode {
                        0 => Ok(true),
                        1 => panic!("injected bootstrap executor panic"),
                        _ => std::future::pending().await,
                    }
                },
                Duration::from_millis(20),
            );
            if mode == 2 {
                worker.0.abort();
            }
            c.tasks.insert(("test-device".into(), 7), worker);
            if mode == 0 {
                c.records
                    .get_mut("test-device")
                    .unwrap()
                    .invalidate(100, "replaced");
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while !c.collect_workers(101) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(c.tasks.is_empty());
            assert!(!c.collect_workers(102));
            assert_eq!(c.records["test-device"].write_pending, Some(7));
            assert!(c.records["test-device"].worker.is_none());
        }
    }

    fn active_record() -> Coordinator {
        Coordinator {
            records: BTreeMap::from([(
                "test-device".into(),
                Record {
                    generation: 7,
                    worker: Some(7),
                    state: Phase::Reading,
                    attempts: 1,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        }
    }

    #[test]
    fn failed_durable_admission_never_dispatches_and_does_not_cache_an_acknowledgement() {
        let mut c = active_record();
        let job = Job {
            id: "test-device".into(),
            generation: 7,
            target: "020000000001".into(),
            url: "http://192.0.2.1/cwmp".into(),
            persist_matching: false,
            vlans: vec![46],
            priority: 0,
            authorization_epoch: 1,
        };
        assert!(c
            .durable_job(Some(job), |_| Err(std::io::Error::other("fault")))
            .is_none());
        assert!(c.records["test-device"].worker.is_none());
        assert_eq!(c.records["test-device"].state, Phase::RetryWait);
        assert!(c.saved.is_empty());
        assert!(c.persistence_error.is_some());
        let mut retried = false;
        assert!(c.persist_with(|_| {
            retried = true;
            Ok(())
        }));
        assert!(retried);
        assert!(c.persistence_error.is_none());
        assert!(
            c.persist_with(|_| panic!("successfully durable unchanged content needs no new write"))
        );
    }

    #[test]
    fn write_phase_requires_all_real_durability_barriers_even_after_identical_rename() {
        use storage::test_io::{self, Point};
        for fault in [Point::FileSync, Point::Rename, Point::DirectorySync] {
            let mut c = active_record();
            let directory = test_io::registry_directory("/etc/fttr/bootstrap-test");
            let path = format!("{directory}/{fault:?}.json");
            test_io::fail_next(fault);
            assert!(!c.advance(
                "test-device",
                7,
                Phase::Configuring,
                100,
                "intent",
                |bytes| storage::write_atomic(&path, bytes, true)
            ));
            assert!(c.saved.is_empty());
            assert_eq!(c.records["test-device"].write_pending, Some(7));
            test_io::events();
            test_io::fail_next(fault);
            assert!(!c.advance(
                "test-device",
                7,
                Phase::Configuring,
                100,
                "intent",
                |bytes| storage::write_atomic(&path, bytes, true)
            ));
            assert!(test_io::events().contains(&fault));
            assert!(c.advance(
                "test-device",
                7,
                Phase::Configuring,
                100,
                "intent",
                |bytes| storage::write_atomic(&path, bytes, true)
            ));
            assert!(!c.saved.is_empty());
        }
    }

    #[test]
    fn unresolved_write_survives_restart_removal_and_obsolete_completion() {
        for completion in [Ok(true), Err("timeout".into())] {
            let mut c = active_record();
            assert!(c.advance(
                "test-device",
                7,
                Phase::ConfiguringManagementWan,
                100,
                "intent",
                |_| Ok(())
            ));
            c.records
                .get_mut("test-device")
                .unwrap()
                .invalidate(101, "identity changed");
            c.records
                .get_mut("test-device")
                .unwrap()
                .finish(7, completion, 102);
            assert_eq!(c.records["test-device"].write_pending, Some(7));
            assert!(c.records["test-device"].settings_verified_at.is_none());
            let bytes = serde_json::to_vec(&c.records).unwrap();
            c.records = serde_json::from_slice(&bytes).unwrap();
            c.restart(103);
            c.configure(&[], 104);
            assert_eq!(c.records["test-device"].write_pending, Some(7));
            assert!(c.records["test-device"].worker.is_none());
        }
        let mut c = active_record();
        c.records.get_mut("test-device").unwrap().state = Phase::Verifying;
        c.restart(100);
        assert_eq!(c.records["test-device"].write_pending, Some(7));
    }

    #[test]
    fn successful_current_completion_clears_only_its_own_intent() {
        let mut c = active_record();
        assert!(
            c.advance("test-device", 7, Phase::Configuring, 100, "intent", |_| Ok(
                ()
            ))
        );
        c.records
            .get_mut("test-device")
            .unwrap()
            .finish(7, Ok(true), 101);
        assert!(c.records["test-device"].write_pending.is_none());
        assert_eq!(c.records["test-device"].settings_verified_at, Some(101));
        assert!(c.persist_with(|_| Ok(())));
    }

    #[test]
    fn unreadable_or_corrupt_state_is_not_overwritten_by_new_observations() {
        let mut c = active_record();
        c.load_blocked = true;
        c.persistence_error = Some("decode failed".into());
        assert!(!c.advance(
            "test-device",
            7,
            Phase::Configuring,
            100,
            "intent",
            |_| panic!("must preserve unreadable state")
        ));
        assert_eq!(c.persistence_error.as_deref(), Some("decode failed"));
    }

    #[test]
    fn failed_changed_snapshot_invalidates_ack_even_when_reverting_to_old_content() {
        let mut c = active_record();
        c.records.get_mut("test-device").unwrap().write_pending = Some(7);
        assert!(c.persist_with(|_| Ok(())));
        c.records.get_mut("test-device").unwrap().write_pending = Some(8);
        assert!(
            !c.persist_with(|_| Err(std::io::Error::other("directory sync failed after rename")))
        );
        c.records.get_mut("test-device").unwrap().write_pending = Some(7);
        let mut wrote = false;
        assert!(c.persist_with(|_| {
            wrote = true;
            Ok(())
        }));
        assert!(
            wrote,
            "a failed barrier invalidates the last acknowledged content cache"
        );
    }

    #[test]
    fn derived_bootstrap_observations_do_not_rewrite_restart_state() {
        let mut c = active_record();
        assert!(c.persist_with(|bytes| {
            assert_eq!(bytes, b"{}");
            Ok(())
        }));
        c.records.get_mut("test-device").unwrap().attempts = 4;
        c.records.get_mut("test-device").unwrap().last_inform = Some(999);
        assert!(c.persist_with(|_| panic!("derived observations stay in memory")));
    }

    #[test]
    fn legacy_unknown_bootstrap_is_imported_as_unresolved_but_new_read_failure_is_not() {
        let mut c = active_record();
        let record = c.records.get_mut("test-device").unwrap();
        record.method = "hl202-mqtt-acs".into();
        record.last_error = Some("unknown timeout".into());
        let mut value = serde_json::to_value(&c.records).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(decode_records(&bytes).unwrap()["test-device"]
            .write_pending
            .is_none());
        value["test-device"]
            .as_object_mut()
            .unwrap()
            .remove("write_pending");
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            decode_records(&bytes).unwrap()["test-device"].write_pending,
            Some(7)
        );
        assert!(decode_records(b"not-json").is_err());
    }

    #[test]
    #[cfg(feature = "device-hl202")]
    fn attempt_budget_and_unknown_write_cannot_be_reset_by_transport_or_desired_changes() {
        let mut c = TestCoordinator::default();
        let d = device("bounded", "hl202");
        let mut m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "020000000001";
        c.sources.observe(mac, 100);
        let job = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        c.records.get_mut(&d.id).unwrap().finish(
            job.generation,
            Err("initial read timeout".into()),
            101,
        );
        c.records.get_mut(&d.id).unwrap().attempts = MAX_ATTEMPTS;
        m.acs_url = "http://192.0.2.2/cwmp".into();
        c.restart(102);
        c.sources.observe(mac, 170);
        assert!(schedule(&mut c, &d, &m, Some(mac), 170, None).is_none());
        assert_eq!(c.records[&d.id].attempts, MAX_ATTEMPTS);
        assert!(c.records[&d.id].reason.contains("budget exhausted"));
        c.records.get_mut(&d.id).unwrap().write_pending = Some(job.generation);
        assert!(schedule(&mut c, &d, &m, Some(mac), 171, Some(171)).is_none());
        assert_eq!(c.records[&d.id].state, Phase::AwaitingApply);
        assert!(c.records[&d.id]
            .reason
            .contains("unresolved bootstrap write"));
    }

    struct TestCoordinator {
        coordinator: Coordinator,
        sources: Registry,
        links: BTreeMap<String, String>,
    }

    #[test]
    #[cfg(feature = "device-hl202")]
    fn bootstrap_capacity_defers_without_spending_attempts_or_allocating_a_worker() {
        let mut c = TestCoordinator::default();
        for id in ["busy-a", "busy-b"] {
            c.records.insert(
                id.into(),
                Record {
                    worker: Some(7),
                    ..Default::default()
                },
            );
        }
        let d = device("waiting", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        c.sources.observe("020000000003", 100);
        assert!(schedule(&mut c, &d, &m, Some("020000000003"), 100, None).is_none());
        assert_eq!(c.records[&d.id].attempts, 0);
        assert!(c.records[&d.id].worker.is_none());
        c.records.get_mut("busy-a").unwrap().worker = None;
        assert!(schedule(&mut c, &d, &m, Some("020000000003"), 101, None).is_some());
        assert_eq!(c.records.values().filter(|r| r.worker.is_some()).count(), 2);
    }
    impl Default for TestCoordinator {
        fn default() -> Self {
            Self {
                coordinator: Coordinator::default(),
                sources: Registry::new("bootstrap-test".into()),
                links: BTreeMap::new(),
            }
        }
    }
    impl std::ops::Deref for TestCoordinator {
        type Target = Coordinator;
        fn deref(&self) -> &Coordinator {
            &self.coordinator
        }
    }
    impl std::ops::DerefMut for TestCoordinator {
        fn deref_mut(&mut self) -> &mut Coordinator {
            &mut self.coordinator
        }
    }
    impl TestCoordinator {
        fn schedule(&mut self, observation: Observation<'_>) -> Option<Job> {
            let onus = self
                .links
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .enumerate()
                .map(|(index, sn)| (u16::try_from(index).unwrap(), sn.clone()))
                .collect();
            self.sources
                .update_topology(onus, Ok(self.links.clone()), observation.time);
            self.coordinator.schedule(observation, &self.sources)
        }
    }
    fn device(id: &str, adapter: &str) -> Device {
        Device {
            id: id.into(),
            optical_sn: "H3CT12345678".into(),
            cwmp_id: id.into(),
            omci_adapter: adapter.into(),
            bootstrap_adapter: if adapter == "hl202" {
                "hl202-mqtt-acs"
            } else {
                "manual-acs"
            }
            .into(),
            provisioning_adapter: "hl202-cu".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            items: Vec::new(),
            wifi_cleanup: "preserve".into(),
            wifi_preserve: Default::default(),
        }
    }
    fn schedule(
        c: &mut TestCoordinator,
        d: &Device,
        m: &Management,
        learned_target: Option<&str>,
        time: u64,
        at: Option<u64>,
    ) -> Option<Job> {
        if let Some(mac) = learned_target {
            c.links.insert(mac.into(), d.optical_sn.clone());
        }
        c.schedule(Observation {
            device: d,
            management: Some(m),
            last_inform: at,
            online: true,
            new_boot: false,
            freshness: 180,
            time,
            acs_available: true,
            authorization: Some((1, true)),
        })
    }
    #[test]
    fn disabling_or_replacing_bootstrap_invalidates_the_worker_before_it_can_publish() {
        let original = device("independent", "hl202");
        for change in ["disabled", "method", "identity", "removed"] {
            let mut c = Coordinator::default();
            c.records.insert(
                original.id.clone(),
                Record {
                    optical_sn: original.optical_sn.clone(),
                    method: original.bootstrap_adapter.clone(),
                    connection_request_enabled: Some(original.connection_request.enabled),
                    generation: 7,
                    worker: Some(7),
                    ..Default::default()
                },
            );
            let mut selected = original.clone();
            selected.provisioning_adapter = "none".into();
            c.configure(&[selected.clone()], 100);
            assert_eq!(c.records[&original.id].generation, 7);
            match change {
                "disabled" => selected.bootstrap_adapter = "none".into(),
                "method" => selected.bootstrap_adapter = "manual-acs".into(),
                "identity" => selected.optical_sn = "H3CT00000002".into(),
                "removed" => (),
                _ => unreachable!(),
            }
            let devices = if change == "removed" {
                vec![]
            } else {
                vec![selected]
            };
            c.configure(&devices, 101);
            let record = c.records.get_mut(&original.id).unwrap();
            assert!(record.removed);
            assert_ne!(record.generation, 7);
            assert_eq!(record.worker, Some(7));
            record.finish(7, Ok(true), 102);
            assert_eq!(record.worker, None);
            assert_eq!(record.settings_verified_at, None);
            c.configure(&devices, 103);
            assert!(!c.records.contains_key(&original.id));
        }
    }
    #[test]
    fn business_adapter_and_binding_changes_preserve_bootstrap_progress() {
        let mut c = TestCoordinator::default();
        let mut d = device("independent", "generic");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        schedule(&mut c, &d, &m, None, 100, None);
        schedule(&mut c, &d, &m, None, 102, Some(101));
        assert_eq!(c.records[&d.id].state, Phase::Ready);
        let generation = c.records[&d.id].generation;
        for adapter in ["none", "hl202-mqtt", "vendor-ct-candidate"] {
            d.provisioning_adapter = adapter.into();
            d.cwmp_id = "bound-native-identity".into();
            schedule(&mut c, &d, &m, None, 103, Some(102));
            assert_eq!(c.records[&d.id].generation, generation);
            assert_eq!(c.records[&d.id].state, Phase::Ready);
        }
    }
    #[test]
    fn manual_bootstrap_observes_without_authorizing_a_remote_write() {
        let mut c = TestCoordinator::default();
        let d = device("manual-readonly", "generic");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        assert!(c
            .schedule(Observation {
                device: &d,
                management: Some(&m),
                last_inform: None,
                online: true,
                new_boot: false,
                freshness: 180,
                time: 100,
                acs_available: true,
                authorization: None
            })
            .is_none());
        assert_eq!(c.records[&d.id].state, Phase::ManualRequired);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn stale_inform_does_not_erase_verified_bootstrap_settings() {
        let mut c = TestCoordinator::default();
        let d = device("stale-inform", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        let first = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(first.generation, Ok(true), 101);
        schedule(&mut c, &d, &m, None, 103, Some(102));
        assert_eq!(c.records[&d.id].state, Phase::Ready);
        // Keep optical/MQTT observation time stable and remove only Inform freshness.
        schedule(&mut c, &d, &m, None, 104, None);
        assert_eq!(c.records[&d.id].state, Phase::WaitingInform);
        assert_eq!(c.records[&d.id].settings_verified_at, Some(101));
        assert_eq!(c.records[&d.id].generation, first.generation);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn registry_admission_cannot_be_bypassed_by_bootstrap() {
        let mut c = TestCoordinator::default();
        let d = device("bounded", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        c.links = (0..64)
            .map(|index| (format!("02000000{index:04X}"), "OTHER12345678".into()))
            .collect();
        c.links.insert("02AABBCCDDEE".into(), d.optical_sn.clone());
        c.sources.update_topology(
            BTreeMap::from([(1, d.optical_sn.clone())]),
            Ok(c.links.clone()),
            100,
        );
        for index in 0..64 {
            c.sources.observe(&format!("02000000{index:04X}"), 100);
        }

        c.sources.observe("02AABBCCDDEE", 100);
        let job = schedule(&mut c, &d, &m, None, 100, None);

        assert!(job.is_none());
        assert_eq!(c.records[&d.id].state, Phase::WaitingIdentity);
        assert_eq!(c.sources.snapshots(100).len(), 64);
    }

    #[cfg(feature = "device-hl202")]
    #[test]
    fn reconnect_invalidates_verified_settings_before_a_fresh_inform_can_mark_ready() {
        let mut c = TestCoordinator::default();
        let d = device("reconnect", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        let first = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(first.generation, Ok(true), 101);
        let epoch = c.records[&d.id].transport_epoch;

        c.sources.observe(mac, 161);
        let next = schedule(&mut c, &d, &m, None, 162, Some(162)).unwrap();

        assert_ne!(next.generation, first.generation);
        assert!(c.records[&d.id].transport_epoch > epoch);
        assert!(c.records[&d.id].settings_verified_at.is_none());
        assert_eq!(c.records[&d.id].state, Phase::Reading);
    }

    #[cfg(feature = "device-hl202")]
    #[test]
    fn restart_discards_persisted_settings_even_when_the_source_epoch_matches() {
        let mut c = TestCoordinator::default();
        let d = device("restart", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        let first = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(first.generation, Ok(true), 101);
        schedule(&mut c, &d, &m, None, 103, Some(102));
        assert_eq!(c.records[&d.id].state, Phase::Ready);
        let bytes = serde_json::to_vec(&c.records).unwrap();
        let mut restarted = TestCoordinator::default();
        restarted.records = serde_json::from_slice(&bytes).unwrap();

        restarted.restart(200);
        restarted.sources.observe(mac, 201);
        let next = schedule(&mut restarted, &d, &m, Some(mac), 202, Some(202)).unwrap();

        assert_ne!(next.generation, first.generation);
        assert_eq!(
            restarted.records[&d.id].transport_epoch,
            c.records[&d.id].transport_epoch
        );
        assert!(restarted.records[&d.id].settings_verified_at.is_none());
    }

    #[cfg(feature = "device-hl202")]
    #[test]
    fn manual_authorization_is_consumed_by_fresh_inform_and_reset_needs_a_new_grant() {
        let mut c = TestCoordinator::default();
        let d = device("manual", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1:7547/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.links.insert(mac.into(), d.optical_sn.clone());
        c.sources.observe(mac, 100);
        let observe = |c: &mut TestCoordinator, time, at, new_boot, epoch| {
            c.schedule(Observation {
                device: &d,
                management: Some(&m),
                last_inform: at,
                online: true,
                new_boot,
                freshness: 180,
                time,
                acs_available: true,
                authorization: Some((epoch, false)),
            })
        };
        let first = observe(&mut c, 100, None, false, 1).unwrap();
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(first.generation, Ok(true), 101);
        assert!(observe(&mut c, 103, Some(102), false, 1).is_none());
        assert_eq!(c.records[&d.id].state, Phase::Ready);
        assert!(observe(&mut c, 104, None, true, 1).is_none());
        assert_eq!(c.records[&d.id].state, Phase::AwaitingApply);
        let next = observe(&mut c, 105, None, false, 2).unwrap();
        assert_eq!(next.authorization_epoch, 2);
        assert_ne!(next.generation, first.generation);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn existing_inform_does_not_skip_mqtt_settings_readback_and_post_write_inform_is_required() {
        let mut c = TestCoordinator::default();
        let d = device("manual", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1:7547/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.links.insert(mac.into(), d.optical_sn.clone());
        c.sources.observe(mac, 100);
        c.records.insert(
            d.id.clone(),
            Record {
                desired_version: format!("{}-1", version(&d, Some(&m), Some(mac))),
                optical_sn: d.optical_sn.clone(),
                initial_inform: Some(Some(90)),
                ..Default::default()
            },
        );
        let first = schedule(&mut c, &d, &m, Some(mac), 100, Some(95))
            .expect("fresh existing Inform must not skip credential readback");
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(first.generation, Ok(true), 101);
        assert!(schedule(&mut c, &d, &m, Some(mac), 102, Some(95)).is_none());
        assert_ne!(
            c.records[&d.id].state,
            Phase::Ready,
            "an Inform predating settings verification cannot confirm the new credentials"
        );
        assert!(schedule(&mut c, &d, &m, Some(mac), 103, Some(102)).is_none());
        assert_eq!(
            c.records[&d.id].state,
            Phase::Ready,
            "a subsequent Inform confirms the verified settings"
        );
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn onu_link_binds_only_unique_fresh_mqtt_source_and_invalidates_reassigned_link() {
        let mut c = TestCoordinator::default();
        let d = device("automatic", "hl202");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        assert!(schedule(&mut c, &d, &m, None, 100, None).is_none());
        assert_eq!(c.records[&d.id].state, Phase::WaitingIdentity);
        c.links.insert(mac.into(), d.optical_sn.clone());
        let job = schedule(&mut c, &d, &m, None, 101, None).unwrap();
        assert_eq!(job.target, mac);
        assert_eq!(c.records[&d.id].binding_source, "onu_link");
        c.links.insert(mac.into(), "OTHER12345678".into());
        assert!(schedule(&mut c, &d, &m, None, 102, None).is_none());
        assert_ne!(c.records[&d.id].generation, job.generation);
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(job.generation, Ok(true), 103);
        assert!(c.records[&d.id].settings_verified_at.is_none());
        schedule(&mut c, &d, &m, None, 104, None);
        assert_eq!(c.records[&d.id].state, Phase::WaitingIdentity);
        c.links.insert(mac.into(), d.optical_sn.clone());
        c.links.insert("02AABBCCDDFF".into(), d.optical_sn.clone());
        c.sources.observe("02AABBCCDDFF", 104);
        assert!(schedule(&mut c, &d, &m, None, 105, None).is_none());
        assert!(c.records[&d.id].reason.contains("ambiguous"));
        c.sources.observe(mac, 170);
        c.sources.update_topology(
            BTreeMap::from([(1, d.optical_sn.clone())]),
            Ok(c.links.clone()),
            170,
        );
        assert_eq!(
            c.sources
                .automatic_source(&d.optical_sn, 170)
                .unwrap()
                .map(|(mac, _)| mac.to_owned()),
            Some(mac.into())
        );
    }

    #[cfg(feature = "device-hl202")]
    #[test]
    fn continuously_online_device_retries_without_overlapping_or_blocking_other_devices() {
        let mut c = TestCoordinator::default();
        let d = device("first", "hl202");
        let mut other = device("second", "hl202");
        other.optical_sn = "H3CT87654321".into();
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 7,
            acs_url: "http://192.0.2.1:7547/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        let first = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        assert!(schedule(&mut c, &d, &m, Some(mac), 101, None).is_none());
        c.sources.observe("02AABBCCDDFF", 101);
        assert!(schedule(&mut c, &other, &m, Some("02AABBCCDDFF"), 101, None).is_some());
        c.records.get_mut("first").unwrap().finish(
            first.generation,
            Err("unknown timeout".into()),
            102,
        );
        assert!(schedule(&mut c, &d, &m, Some(mac), 106, None).is_none());
        c.sources.observe(mac, 107);
        let retry = schedule(&mut c, &d, &m, Some(mac), 107, None).unwrap();
        assert_eq!(c.records["first"].attempts, 2);
        assert!(retry.persist_matching);
        c.records
            .get_mut("first")
            .unwrap()
            .finish(retry.generation, Ok(true), 108);
        assert_eq!(c.records["first"].state, Phase::WaitingInform);
        assert!(schedule(&mut c, &d, &m, Some(mac), 110, Some(109)).is_none());
        assert_eq!(c.records["first"].state, Phase::Ready);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn new_configuration_waits_for_old_worker_then_uses_new_url_and_discards_old_result() {
        let mut c = TestCoordinator::default();
        let d = device("first", "hl202");
        let mut m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.sources.observe(mac, 100);
        let old = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        m.acs_url = "http://192.0.2.2/cwmp".into();
        assert!(schedule(&mut c, &d, &m, Some(mac), 101, None).is_none());
        c.records
            .get_mut("first")
            .unwrap()
            .finish(old.generation, Ok(true), 102);
        assert!(c.records["first"].settings_verified_at.is_none());
        let new = schedule(&mut c, &d, &m, Some(mac), 103, None).unwrap();
        assert_ne!(old.generation, new.generation);
        assert_eq!(new.url, m.acs_url);
    }
    #[test]
    fn manual_device_becomes_ready_without_mqtt_and_clock_rollback_cannot_reuse_old_inform() {
        let mut c = TestCoordinator::default();
        let d = device("manual", "hwtc");
        let m = Management {
            vlans: vec![46],
            vlan: 46,
            priority: 4,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        assert!(schedule(&mut c, &d, &m, None, 110, Some(100)).is_none());
        assert_eq!(c.records["manual"].state, Phase::ManualRequired);
        assert!(schedule(&mut c, &d, &m, None, 112, Some(111)).is_none());
        assert_eq!(c.records["manual"].state, Phase::Ready);
        schedule(&mut c, &d, &m, None, 90, Some(111));
        assert_eq!(c.records["manual"].state, Phase::ManualRequired);
        schedule(&mut c, &d, &m, None, 92, Some(91));
        assert_eq!(c.records["manual"].state, Phase::Ready);
    }
    #[test]
    fn restored_inform_is_not_fresh_and_devices_have_independent_evidence() {
        let first = Record {
            initial_inform: Some(Some(100)),
            require_inform_after: 110,
            ..Record::default()
        };
        let second = Record {
            initial_inform: Some(None),
            ..Record::default()
        };
        assert!(!first.fresh_inform(Some(100), 120, 180, true));
        assert!(!first.fresh_inform(Some(109), 120, 180, true));
        assert!(first.fresh_inform(Some(111), 120, 180, true));
        assert!(!first.fresh_inform(Some(111), 120, 180, false));
        assert!(second.fresh_inform(Some(109), 120, 180, true));
    }
    #[test]
    fn invalidation_retains_worker_slot_until_completion() {
        let mut r = Record {
            generation: 3,
            worker: Some(3),
            settings_verified_at: Some(10),
            ..Record::default()
        };
        r.invalidate(20, "reset");
        assert_eq!(r.generation, 4);
        assert_eq!(r.worker, Some(3));
        assert_eq!(r.settings_verified_at, None);
        let recovered: Record = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(recovered.worker, None);
        assert_eq!(recovered.initial_inform, None);
    }
    #[test]
    fn retry_is_bounded_and_mac_mapping_is_generic() {
        assert_eq!(retry_delay(1), 5);
        assert_eq!(retry_delay(3), 20);
        assert_eq!(retry_delay(u32::MAX), 300);
        assert_eq!(
            normalize_target("02aabbccddee/fttr_data.tar.gz"),
            Some("02AABBCCDDEE".into())
        );
        assert!(normalize_target("H3CT12345678").is_none());
        assert!(normalize_target("../02AABBCCDDEE").is_none());
        assert_eq!(
            public_url("http://user:secret@192.0.2.1:7547/cwmp"),
            "http://192.0.2.1:7547/cwmp"
        );
    }
}
