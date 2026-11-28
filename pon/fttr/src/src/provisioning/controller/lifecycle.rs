use super::*;
use serde::ser::{Serialize, SerializeSeq, Serializer};
use serde::Deserialize;

pub(super) fn validate_desired_pins(
    resources: &crate::platform::resources::Resources,
    config: &Config,
) -> Result<(), String> {
    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    if std::ptr::eq(
        resources,
        crate::platform::fiberhome_an4475_g2::active_resources(),
    ) {
        let count = usize::from(config.fiberhome_onu_count.unwrap_or(128));
        return crate::platform::fiberhome_an4475_g2::resources_for_count(count)
            .validate_pins(&config.onu_pins);
    }
    resources.validate_pins(&config.onu_pins)
}

#[derive(Deserialize)]
struct Recovered {
    operations: Vec<Value>,
    #[serde(default)]
    retired: Vec<Value>,
    #[serde(default)]
    owned: BTreeMap<String, BTreeMap<String, wan::Owned>>,
}

struct Operations<'a>(&'a Controller);

impl Serialize for Operations<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (id, recovered) in &self.0.recovered {
            if !self.0.operations.contains_key(id) {
                sequence.serialize_element(recovered)?;
            }
        }
        for operation in self.0.operations.values() {
            sequence.serialize_element(&operation.view())?;
        }
        sequence.end()
    }
}

impl Controller {
    fn same_transport(previous: &Device, next: &Device) -> bool {
        let a = &previous.connection_request;
        let b = &next.connection_request;
        previous.id == next.id
            && previous.optical_sn == next.optical_sn
            && (previous.cwmp_id == next.cwmp_id || next.cwmp_id.starts_with("pending:"))
            && previous.omci_adapter == next.omci_adapter
            && previous.bootstrap_adapter == next.bootstrap_adapter
            && previous.provisioning_adapter == next.provisioning_adapter
            && a.enabled == b.enabled
            && a.defaults == b.defaults
            && a.username == b.username
            && a.password == b.password
            && a.port == b.port
    }

    fn independent_business_reload(&self, next: &Config) -> bool {
        fn same<T: serde::Serialize>(a: &T, b: &T) -> bool {
            matches!((serde_json::to_vec(a), serde_json::to_vec(b)), (Ok(a), Ok(b)) if a == b)
        }
        // Missing generation evidence is not authority to bypass the shared
        // resource barrier. All normal compiled configurations supply it.
        !self.config.devices.is_empty()
            && self.config.device_revisions.len() == self.config.devices.len()
            && next.device_revisions.len() == next.devices.len()
            && self.config.devices.len() == next.devices.len()
            && self.config.devices.iter().all(|previous| {
                next.devices
                    .iter()
                    .any(|d| Self::same_transport(previous, d))
            })
            && self.config.enabled == next.enabled
            && self.config.onu_pins == next.onu_pins
            && self.config.fiberhome_onu_count == next.fiberhome_onu_count
            && self.config.allow_unsupported_as_bridge == next.allow_unsupported_as_bridge
            && self.config.omci_auth == next.omci_auth
            && self.config.mqtt_bridge_enabled == next.mqtt_bridge_enabled
            && self.config.acs_bridge == next.acs_bridge
            && same(&self.config.management, &next.management)
            && same(&self.config.management_network, &next.management_network)
    }

    fn unresolved_binding_change(&self, next: &Config) -> bool {
        self.config.devices.iter().any(|previous| {
            self.previous_facts(&previous.id)
                .is_some_and(|facts| facts.unresolved)
                && !next
                    .devices
                    .iter()
                    .any(|d| Self::same_transport(previous, d))
        })
    }

    fn activate_without_drain(&self, next: &Config) -> bool {
        self.independent_business_reload(next)
            || (self.config.device_revisions.is_empty()
                && next.device_revisions.is_empty()
                && next.business_revision.is_some()
                && next.business_revision == self.config.business_revision)
    }

    pub(super) fn begin_history_tick(&mut self) {
        self.dirty |= self.operations.values().any(|op| {
            op.stage != OperationPhase::Completed && op.stage != OperationPhase::Interrupted
        });
    }

    pub(super) fn persist_history(&mut self, write: impl FnOnce(&[u8]) -> std::io::Result<()>) {
        // Production recovery has one durable authority. Rendering/writing the
        // old UI cache duplicates the same histories and cannot settle a write.
        if self.snapshots.is_some() {
            self.saved.clear();
            self.dirty = false;
            return;
        }
        if !self.dirty && !self.saved.is_empty() {
            return;
        }
        let saved = self.history();
        if saved == self.saved {
            self.dirty = false;
            return;
        }
        match write(saved.as_bytes()) {
            Ok(()) => {
                self.saved = saved;
                self.dirty = false;
                self.failure = None;
            }
            Err(error) => {
                self.failure = Some(format!("provisioning state persistence: {error}"));
                self.dirty = true;
            }
        }
    }

    pub(super) fn resolve_pending_identities(&mut self, devices: &DeviceObservations) {
        let mut resolved = Vec::new();
        for (index, d) in self.config.devices.iter().enumerate() {
            if d.cwmp_id.starts_with("pending:") && !self.resets.blocks(&d.optical_sn) {
                if let Some(id) = discovered_identity(devices, &d.optical_sn) {
                    resolved.push((index, id));
                }
            }
        }
        if !resolved.is_empty() {
            for (index, id) in resolved {
                self.config.devices[index].cwmp_id = id.into();
            }
            crate::omci_auth::bind(&self.config.devices);
            self.retain_configured_state();
        }
    }

    pub(super) fn history(&self) -> String {
        #[derive(serde::Serialize)]
        struct History<'a> {
            operations: Operations<'a>,
            retired: &'a Vec<Value>,
            owned: &'a BTreeMap<String, BTreeMap<String, wan::Owned>>,
        }
        #[expect(
            clippy::expect_used,
            reason = "JSON values and string-keyed ownership maps serialize infallibly"
        )]
        serde_json::to_string(&History {
            operations: Operations(self),
            retired: &self.retired,
            owned: &self.owned,
        })
        .expect("controller history is serializable")
    }

    pub(super) fn recover(&mut self, data: &[u8]) {
        if let Ok(recovered) = serde_json::from_slice::<Recovered>(data) {
            self.retired = recovered.retired;
            self.owned = recovered.owned;
            for mut operation in recovered.operations {
                self.next_id = self
                    .next_id
                    .max(operation["operation_id"].as_u64().unwrap_or(0));
                if operation["optical_sn"]
                    .as_str()
                    .is_some_and(|serial| self.resets.blocks(serial))
                {
                    continue;
                }
                if operation["state"] == "completed" {
                    // Legacy history has no completion timestamp. Start a full wait
                    // at recovery rather than treating the operation start as its end.
                    if operation["completed_at"].as_u64().is_none() {
                        operation["completed_at"] = json!(now());
                    }
                } else {
                    operation["state"] = json!("interrupted");
                    operation["reconcile_required"] = json!(true);
                    operation["reconcile_reason"] = json!("service_restarted");
                    if let Some(items) = operation["items"].as_array_mut() {
                        for item in items {
                            if item["state"] != "verified" && item["state"] != "unknown" {
                                item["state"] = json!("reconcile_required");
                                item["reason"] = json!("reconcile after service restart");
                            }
                        }
                    }
                }
                if let Some(id) = operation["device"].as_str().map(str::to_owned) {
                    self.recovered.insert(id, operation);
                }
            }
            self.owned.retain(|id, _| {
                !self.resets.owns(id)
                    && !self
                        .config
                        .devices
                        .iter()
                        .any(|d| &d.cwmp_id == id && self.resets.blocks(&d.optical_sn))
            });
            self.dirty = true;
        }
    }

    fn config_blocked(&self) -> bool {
        self.operations.values().any(|op| {
            !matches!(
                op.stage,
                OperationPhase::Completed | OperationPhase::Interrupted
            )
        }) || self
            .pending_config
            .as_ref()
            .is_some_and(|(next, _)| self.unresolved_binding_change(next))
            || self
                .config
                .devices
                .iter()
                .any(|d| self.resets.blocks(&d.optical_sn))
    }

    pub(super) fn request_config(&mut self, config: impl Into<Config>) {
        let config = config.into();
        if let Some(resources) = self.resources {
            if let Err(error) = validate_desired_pins(resources, &config) {
                self.config_error = Some(error);
                return;
            }
        }
        // Credential revocation cannot wait for a business transaction to drain.
        // Adapter changes still follow business configuration activation below.
        let mut auth = config.omci_auth.clone();
        auth.selections = self.config.omci_auth.selections.clone();
        if let Err(error) = crate::omci_auth::configure(&auth) {
            self.config_error = Some(error);
            return;
        }
        self.config.omci_auth = auth;
        if config.revision == self.config.revision {
            self.pending_config = None;
            self.config_error = None;
            return;
        }
        // A/bootstrap/host settings do not supersede an unrelated business transaction.
        if self.activate_without_drain(&config)
            && !self
                .config
                .devices
                .iter()
                .any(|d| self.resets.blocks(&d.optical_sn))
        {
            self.pending_config = None;
            self.config_error = self.replace_config(config).err();
            return;
        }
        // Never wait for an offline device's old, entirely unstarted plan.
        // Once discovery or a transport job starts, its owner must drain it.
        for op in self.operations.values_mut().filter(|op| {
            op.unstarted()
                && matches!(
                    op.stage,
                    OperationPhase::Waiting | OperationPhase::WanReconciling
                )
        }) {
            op.supersede_remaining();
            self.dirty = true;
        }
        // Repeated reloads of the same revision retain the original wait time.
        let requested_at = self
            .pending_config
            .as_ref()
            .filter(|(previous, _)| previous.revision == config.revision)
            .map_or_else(now, |(_, at)| *at);
        self.pending_config = Some((config, requested_at));
        self.config_error = None;
        self.activate_pending_config();
    }

    pub(super) fn activate_pending_config(&mut self) -> bool {
        if self.pending_config.is_none() || self.config_blocked() {
            return false;
        }
        let (config, _) = self.pending_config.take().unwrap();
        // The controller mutex covers both the eligibility check and replacement.
        if let Err(error) = self.replace_config(config.clone()) {
            self.pending_config = Some((config, now()));
            self.config_error = Some(error);
            return false;
        }
        self.config_error = None;
        self.failure = None;
        true
    }

    pub(super) fn config_load_view(&self) -> Value {
        let mut blockers: Vec<_> = self.operations.values()
            .filter(|op| !matches!(op.stage, OperationPhase::Completed | OperationPhase::Interrupted))
            .map(|op| json!({"operation_id":op.id,"device":op.device.id,"state":op.stage,"started_at":op.started}))
            .collect();
        if let Some((next, _)) = &self.pending_config {
            for previous in &self.config.devices {
                if self
                    .previous_facts(&previous.id)
                    .is_some_and(|facts| facts.unresolved)
                    && !next
                        .devices
                        .iter()
                        .any(|d| Self::same_transport(previous, d))
                    && !blockers.iter().any(|v| v["device"] == previous.id)
                {
                    blockers.push(json!({"device":previous.id,"state":"unresolved","reason":"configuration would remove or rebind an unresolved transaction owner"}));
                }
            }
        }
        json!({"state":if self.pending_config.is_some() {"waiting"} else {"applied"},
            "requested_revision":self.pending_config.as_ref().map_or(&self.config.revision, |(cfg, _)| &cfg.revision),
            "requested_at":self.pending_config.as_ref().map(|(_, at)| at),
            "blocking_operations":blockers,
            "reset_pending":self.config.devices.iter().any(|d| self.resets.blocks(&d.optical_sn))})
    }

    pub(super) fn replace_config(&mut self, config: impl Into<Config>) -> Result<(), String> {
        let mut config = config.into();
        if let Some(resources) = self.resources {
            validate_desired_pins(resources, &config)?;
        }
        if self.pending_config.is_some() {
            return Err("configuration reload pending; wait for automatic activation".into());
        }
        if self.unresolved_binding_change(&config) {
            return Err("unresolved prior write: configuration cannot remove or rebind its transaction owner".into());
        }
        if self
            .config
            .devices
            .iter()
            .any(|d| self.resets.blocks(&d.optical_sn))
        {
            return Err("device reset pending; retry reset before reload".into());
        }
        if !self.activate_without_drain(&config)
            && self.operations.values().any(|op| {
                !matches!(
                    op.stage,
                    OperationPhase::Completed | OperationPhase::Interrupted
                )
            })
        {
            return Err("active synchronization: finish operation before reload".into());
        }
        for device in &mut config.devices {
            if device.cwmp_id.starts_with("pending:") {
                if let Some(previous) = self.config.devices.iter().find(|previous| {
                    previous.id == device.id && previous.optical_sn == device.optical_sn
                }) {
                    device.cwmp_id.clone_from(&previous.cwmp_id);
                }
            }
            if let Some(previous) = self
                .config
                .devices
                .iter()
                .find(|previous| Self::same_transport(previous, device))
            {
                // Preserve live connection-request authorization for an unchanged
                // transport. Old operations still own this exact policy snapshot.
                device.connection_request.authorization =
                    previous.connection_request.authorization.clone();
            }
        }
        crate::omci_auth::configure(&config.omci_auth)?;
        for previous in &self.config.devices {
            if !config.devices.iter().any(|d| {
                d.id == previous.id
                    && d.optical_sn == previous.optical_sn
                    && d.cwmp_id == previous.cwmp_id
            }) {
                self.management.remove(&previous.id);
                self.mqtt.remove(&previous.id);
                self.last_boot.remove(&previous.id);
                self.last_online.remove(&previous.id);
            }
        }
        for previous in &self.config.devices {
            if config.devices.iter().any(|d| {
                std::sync::Arc::ptr_eq(
                    &d.connection_request.authorization,
                    &previous.connection_request.authorization,
                )
            }) {
                continue;
            }
            previous
                .connection_request
                .authorization
                .store(false, std::sync::atomic::Ordering::Release);
        }
        crate::omci_auth::bind(&config.devices);
        crate::controller::mqtt_bridge::configure(config.mqtt_bridge_enabled);
        crate::controller::acs_bridge::configure(&config.acs_bridge);
        self.config = config.into();
        super::super::bootstrap::configure(&self.config.devices);
        super::super::health_watch::config(
            self.config
                .devices
                .iter()
                .filter(|d| d.omci_management())
                .map(|d| d.optical_sn.clone())
                .collect(),
        );
        self.load_reset_intents();
        self.retain_configured_state();
        Ok(())
    }

    pub(super) fn retain_configured_state(&mut self) {
        let previous = (
            self.operations.len(),
            self.recovered.len(),
            self.owned.len(),
        );
        let devices = &self.config.devices;
        for (id, view) in self
            .operations
            .iter()
            .filter(|(_, op)| op.observes().is_none() && op.recovers().is_none())
            .map(|(id, op)| (id, op.view()))
            .chain(self.recovered.iter().map(|(id, op)| (id, op.clone())))
        {
            if facts::Facts::recovered(&view).unresolved {
                continue;
            }
            if devices.iter().any(|d| {
                &d.id == id
                    && view["optical_sn"] == d.optical_sn
                    && view["provisioning_adapter"] != d.provisioning_adapter
            }) {
                let mut retired = view;
                retired["retired_reason"] =
                    json!("adapter selection changed; rediscovery required");
                self.retired.push(retired);
            }
        }
        if self.retired.len() > 32 {
            self.retired.drain(..self.retired.len() - 32);
        }
        self.operations.retain(|id, op| {
            op.facts().unresolved
                || devices.iter().any(|d| {
                    &d.id == id
                        && d.backend() == op.device.backend()
                        && d.provisioning_adapter == op.device.provisioning_adapter
                        && d.optical_sn == op.device.optical_sn
                        && (d.cwmp_id.starts_with("pending:")
                            || op.device.cwmp_id.starts_with("pending:")
                            || d.cwmp_id == op.device.cwmp_id)
                })
        });
        self.recovered.retain(|id, op| {
            !self
                .operations
                .get(id)
                .is_some_and(|active| active.observes().is_none() && active.recovers().is_none())
                && (facts::Facts::recovered(op).unresolved
                    || devices.iter().any(|d| {
                        &d.id == id
                            && op["optical_sn"] == d.optical_sn
                            && op["backend"]["effective"].as_str().unwrap_or("acs") == d.backend()
                            && op["provisioning_adapter"] == d.provisioning_adapter
                            && (d.cwmp_id.starts_with("pending:") || op["cwmp_id"] == d.cwmp_id)
                    }))
        });
        self.observations
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.management
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.mqtt
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.last_boot
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.last_online
            .retain(|id| devices.iter().any(|d| &d.id == id));
        self.omci_management
            .retain(|serial, _| devices.iter().any(|d| &d.optical_sn == serial));
        self.optical_epochs
            .retain(|serial, _| devices.iter().any(|d| &d.optical_sn == serial));
        self.attempts
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.next_retry_at
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        self.automatic_ready
            .retain(|id, _| devices.iter().any(|d| &d.id == id));
        let retained_binding = |id: &String| {
            devices.iter().any(|d| &d.cwmp_id == id)
                || self
                    .recovered
                    .values()
                    .any(|op| op["cwmp_id"].as_str() == Some(id.as_str()))
                || self.operations.values().any(|op| &op.device.cwmp_id == id)
        };
        self.owned.retain(|id, _| retained_binding(id));
        self.wifi_owned.retain(|id, _| retained_binding(id));
        self.dirty |= previous
            != (
                self.operations.len(),
                self.recovered.len(),
                self.owned.len(),
            );
    }
}
