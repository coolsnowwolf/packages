use super::*;
use crate::provisioning::storage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    optical_sn: String,
    cwmp_id: String,
}

pub(super) struct Resets {
    pending: BTreeMap<String, Intent>,
    blocked: BTreeMap<String, String>,
    directory: String,
    history: String,
    #[cfg(test)]
    wan: String,
    #[cfg(test)]
    wifi: String,
}

impl Default for Resets {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            blocked: BTreeMap::new(),
            directory: "/tmp/fttr/state/reset-pending".into(),
            history: crate::provisioning::runtime_store::LEGACY_HISTORY.into(),
            #[cfg(test)]
            wan: "/etc/fttr/wan-owned".into(),
            #[cfg(test)]
            wifi: "/etc/fttr/wifi-owned".into(),
        }
    }
}

impl Resets {
    pub(super) fn blocks(&self, serial: &str) -> bool {
        self.pending.contains_key(serial) || self.blocked.contains_key(serial)
    }
    pub(super) fn owns(&self, id: &str) -> bool {
        self.pending.values().any(|intent| intent.cwmp_id == id)
    }
    pub(super) fn reason(&self, serial: &str) -> Option<&str> {
        self.blocked.get(serial).map(String::as_str)
    }
    fn path(&self, serial: &str) -> String {
        #[cfg(not(test))]
        return crate::provisioning::runtime_store::Record::Reset.path(serial);
        #[cfg(test)]
        storage::registry_path(&self.directory, serial)
    }
}

impl Controller {
    pub(super) fn load_reset_intents(&mut self) {
        for device in &self.config.devices {
            let result = (|| -> Result<Option<Intent>, String> {
                let data = match storage::read(self.resets.path(&device.optical_sn)) {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(format!("read reset intent: {e}")),
                };
                if data.len() > 4096 {
                    return Err("oversized reset intent".into());
                }
                let intent: Intent =
                    serde_json::from_slice(&data).map_err(|_| "invalid reset intent")?;
                if intent.optical_sn != device.optical_sn
                    || intent.cwmp_id.is_empty()
                    || (intent.cwmp_id.starts_with("pending:") && device.backend() != "command")
                    || (!device.cwmp_id.starts_with("pending:") && intent.cwmp_id != device.cwmp_id)
                {
                    return Err("reset intent identity mismatch".into());
                }
                Ok(Some(intent))
            })();
            match result {
                Ok(Some(intent)) => {
                    self.resets
                        .pending
                        .insert(device.optical_sn.clone(), intent);
                }
                Ok(None) => (),
                Err(error) => {
                    self.resets.blocked.insert(device.optical_sn.clone(), error);
                }
            }
        }
    }

    pub(super) fn resume_resets(&mut self) {
        let serials: Vec<_> = self.resets.pending.keys().cloned().collect();
        for serial in serials {
            if let Err(error) = self.finish_reset(&serial) {
                self.resets.blocked.insert(serial, error);
            }
        }
    }

    pub(super) fn reset_device(&mut self, target: &str) -> Result<Value, String> {
        let device = self
            .config
            .devices
            .iter()
            .find(|d| d.id == target || d.optical_sn == target)
            .cloned()
            .ok_or("select one configured device to reset")?;
        if self
            .operations
            .get(&device.id)
            .is_some_and(Operation::command_running)
        {
            return Err(
                "command transaction active; wait for completion before resetting state".into(),
            );
        }
        if !self.resets.pending.contains_key(&device.optical_sn) {
            if self.resets.blocks(&device.optical_sn) {
                return Err("reset pending: invalid intent requires operator repair".into());
            }
            let cwmp_id = if device.cwmp_id.starts_with("pending:") && device.backend() != "command"
            {
                self.recovered
                    .get(&device.id)
                    .and_then(|op| op["cwmp_id"].as_str())
                    .ok_or("resolve device identity before reset")?
                    .to_owned()
            } else {
                device.cwmp_id.clone()
            };
            if cwmp_id.starts_with("pending:") && device.backend() != "command" {
                return Err("resolve device identity before reset".into());
            }
            let intent = Intent {
                optical_sn: device.optical_sn.clone(),
                cwmp_id,
            };
            self.resets
                .pending
                .insert(device.optical_sn.clone(), intent);
        }
        match self.finish_reset(&device.optical_sn) {
            Ok(()) => Ok(json!({"reset":true,"device":device.id})),
            Err(error) => {
                self.resets.blocked.insert(device.optical_sn, error.clone());
                Err(format!("reset pending: {error}"))
            }
        }
    }

    fn finish_reset(&mut self, serial: &str) -> Result<(), String> {
        let intent = self
            .resets
            .pending
            .get(serial)
            .cloned()
            .ok_or("missing reset intent")?;
        let data =
            serde_json::to_vec(&intent).map_err(|e| format!("serialize reset intent: {e}"))?;
        storage::write_atomic(&self.resets.path(serial), &data, true)
            .map_err(|e| format!("persist reset intent: {e}"))?;
        #[cfg(feature = "acs")]
        {
            let reply = acs(super::Command::ResetDeviceTasks {
                device: intent.cwmp_id.clone(),
            });
            if reply["reset"] != true {
                return Err(reply["error"]
                    .as_str()
                    .unwrap_or("ACS reset not acknowledged")
                    .into());
            }
        }
        self.forget_device(serial, &intent.cwmp_id);
        #[cfg(not(test))]
        storage::remove_registry(&crate::provisioning::runtime_store::Record::Native.path(serial))?;
        if self.snapshots.is_none() {
            let saved = self.history();
            storage::write_atomic(&self.resets.history, saved.as_bytes(), true)
                .map_err(|e| format!("persist reset history: {e}"))?;
            self.saved = saved;
        } else {
            self.saved.clear();
        }
        self.dirty = false;
        #[cfg(not(test))]
        {
            wan::reset(&intent.cwmp_id)?;
            wifi::reset(&intent.cwmp_id)?;
        }
        #[cfg(test)]
        {
            storage::remove_registry(&storage::registry_path(&self.resets.wan, &intent.cwmp_id))?;
            storage::remove_registry(&storage::registry_path(&self.resets.wifi, &intent.cwmp_id))?;
        }
        if !self.persist_snapshots() {
            return Err("persist reset recovery snapshot failed".into());
        }
        self.complete_reset(serial)
    }

    fn complete_reset(&mut self, serial: &str) -> Result<(), String> {
        storage::remove_registry(&self.resets.path(serial))?;
        self.resets.pending.remove(serial);
        self.resets.blocked.remove(serial);
        Ok(())
    }
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;
