use super::{automatic_target, normalize_target, Selection, Snapshot, Transport, FRESHNESS};
use std::collections::BTreeMap;

pub struct Registry {
    daemon_epoch: String,
    topology_epoch: u64,
    source_epoch: u64,
    clock: u64,
    topology_seen: Option<u64>,
    topology_error: Option<String>,
    sources: BTreeMap<String, Transport>,
    onus: BTreeMap<u16, String>,
    links: BTreeMap<String, String>,
    verified_reports: BTreeMap<String, String>,
}

impl Registry {
    pub fn new(daemon_epoch: String) -> Self {
        Self {
            daemon_epoch,
            topology_epoch: 0,
            source_epoch: 0,
            clock: 0,
            topology_seen: None,
            topology_error: None,
            sources: BTreeMap::new(),
            onus: BTreeMap::new(),
            links: BTreeMap::new(),
            verified_reports: BTreeMap::new(),
        }
    }

    fn advance(&mut self, time: u64) {
        if time < self.clock {
            self.sources.clear();
            self.links.clear();
            self.topology_seen = None;
            self.topology_epoch = self.topology_epoch.saturating_add(1);
        }
        self.clock = time;
        self.sources
            .retain(|_, source| source.last_seen <= time && time - source.last_seen <= FRESHNESS);
    }

    pub fn observe(&mut self, topic: &str, time: u64) {
        let Some(mac) = normalize_target(topic) else {
            return;
        };
        self.advance(time);
        if !self.sources.contains_key(&mac) && self.sources.len() >= 64 {
            let oldest = self
                .sources
                .iter()
                .filter(|(source, _)| !self.links.contains_key(*source))
                .min_by_key(|(_, t)| t.last_seen)
                .map(|(source, _)| source.clone());
            match oldest {
                Some(source) => {
                    self.sources.remove(&source);
                }
                None => return,
            }
        }
        let observation = self.sources.entry(mac).or_default();
        if observation.epoch == 0 || time.saturating_sub(observation.last_seen) > FRESHNESS {
            self.source_epoch = self.source_epoch.saturating_add(1);
            observation.epoch = self.source_epoch;
        }
        observation.last_seen = time;
    }

    pub fn update_topology(
        &mut self,
        onus: BTreeMap<u16, String>,
        observation: Result<BTreeMap<String, String>, String>,
        time: u64,
    ) {
        self.advance(time);
        let links = match observation {
            Ok(links) => {
                self.topology_error = None;
                links
            }
            Err(error) => {
                self.topology_error = Some(error);
                BTreeMap::new()
            }
        };
        if self.onus != onus || self.links != links {
            self.topology_epoch = self.topology_epoch.saturating_add(1);
        }
        self.onus = onus;
        self.links = links;
        self.topology_seen = Some(time);
    }

    pub fn snapshots(&self, time: u64) -> Vec<Snapshot> {
        self.sources
            .iter()
            .map(|(mac, source)| {
                let serial = self.links.get(mac);
                let mut onus = self.onus.iter().filter(|(_, sn)| Some(*sn) == serial);
                let onu = match (onus.next(), onus.next()) {
                    (Some((id, _)), None) => Some(*id),
                    _ => None,
                };
                let fresh = time >= self.clock
                    && source.last_seen <= time
                    && time - source.last_seen <= FRESHNESS
                    && self
                        .topology_seen
                        .is_some_and(|seen| seen <= time && time - seen <= FRESHNESS);
                let reason = match (fresh, serial, onu) {
                    (false, _, _) => "stale",
                    (true, Some(sn), Some(_)) => {
                        match automatic_target(&self.sources, &self.links, (sn, time)) {
                            Ok(Some(_)) => "ready",
                            Err(_) => "ambiguous",
                            Ok(None) => "unmatched",
                        }
                    }
                    _ => "unmatched",
                };
                Snapshot {
                    mqtt_mac: mac.clone(),
                    optical_sn: serial.cloned(),
                    onu_id: onu,
                    binding_generation: format!(
                        "{}-{}-{}-{mac}",
                        self.daemon_epoch, self.topology_epoch, source.epoch
                    ),
                    last_seen: source.last_seen,
                    reason: reason.into(),
                }
            })
            .collect()
    }

    pub(crate) fn automatic_source(
        &self,
        serial: &str,
        time: u64,
    ) -> Result<Option<(&str, &Transport)>, &'static str> {
        if time < self.clock
            || self
                .topology_seen
                .is_none_or(|seen| seen > time || time - seen > FRESHNESS)
        {
            return Ok(None);
        }
        let mut onus = self.onus.values().filter(|sn| sn.as_str() == serial);
        match (onus.next(), onus.next()) {
            (Some(_), None) => automatic_target(&self.sources, &self.links, (serial, time)),
            _ => Ok(None),
        }
    }

    pub(crate) fn last_seen(&self, target: &str) -> Option<u64> {
        self.sources.get(target).map(|source| source.last_seen)
    }

    pub(crate) fn clock(&self) -> u64 {
        self.clock
    }

    #[cfg_attr(not(feature = "mqtt-bridge"), allow(dead_code))]
    pub(crate) fn fdb_candidates_for_serial(&self, serial: &str, time: u64) -> Vec<String> {
        if time < self.clock
            || self
                .topology_seen
                .is_none_or(|seen| seen > time || time - seen > FRESHNESS)
            || self
                .onus
                .values()
                .filter(|sn| sn.as_str() == serial)
                .count()
                != 1
        {
            return Vec::new();
        }
        self.links
            .iter()
            .filter(|(_, sn)| sn.as_str() == serial)
            .map(|(mac, _)| mac.clone())
            .collect()
    }

    #[cfg_attr(not(feature = "mqtt-bridge"), allow(dead_code))]
    pub(crate) fn seed_verified_report(&mut self, serial: &str, mac: &str) -> bool {
        if serial.len() != 12 || !serial.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
        let Some(mac) = super::normalize_target(mac) else {
            return false;
        };
        if u8::from_str_radix(&mac[..2], 16)
            .ok()
            .is_none_or(|b| b & 1 != 0)
            || mac == "000000000000"
        {
            return false;
        }
        let serial = serial.to_ascii_uppercase();
        if self
            .verified_reports
            .get(&serial)
            .is_some_and(|old| old != &mac)
            || self
                .verified_reports
                .iter()
                .any(|(sn, old)| sn != &serial && old == &mac)
        {
            self.verified_reports
                .retain(|sn, old| sn != &serial && old != &mac);
            return false;
        }
        if !self.verified_reports.contains_key(&serial) && self.verified_reports.len() >= 64 {
            return false;
        }
        self.verified_reports.insert(serial, mac);
        true
    }

    pub(crate) fn verified_reports_snapshot(&self) -> BTreeMap<String, String> {
        self.verified_reports.clone()
    }

    /// Record only a report whose own serial and MAC agree with a fresh,
    /// unambiguous optical/FDB association. This is evidence, not permission
    /// to install a redirect; callers must revalidate current topology and lease.
    pub(crate) fn record_verified_report(&mut self, serial: &str, mac: &str, time: u64) -> bool {
        if self
            .automatic_source(serial, time)
            .ok()
            .flatten()
            .map(|(m, _)| m)
            != Some(mac)
        {
            return false;
        }
        if self
            .verified_reports
            .get(serial)
            .is_some_and(|old| old != mac)
            || self
                .verified_reports
                .iter()
                .any(|(sn, old)| sn != serial && old == mac)
        {
            self.verified_reports
                .retain(|sn, old| sn != serial && old != mac);
            return false;
        }
        if !self.verified_reports.contains_key(serial) && self.verified_reports.len() >= 64 {
            return false;
        }
        self.verified_reports
            .insert(serial.to_owned(), mac.to_owned());
        true
    }

    /// A historical report is usable as a candidate only while the current
    /// optical registration and fresh FDB still agree. DHCP is checked separately.
    #[allow(dead_code)] // Used by the bridge reconciler when enabled.
    pub(crate) fn verified_report_candidate(&self, serial: &str, time: u64) -> Option<&str> {
        if time < self.clock
            || self
                .topology_seen
                .is_none_or(|seen| seen > time || time - seen > FRESHNESS)
            || self
                .onus
                .values()
                .filter(|sn| sn.as_str() == serial)
                .count()
                != 1
        {
            return None;
        }
        let mac = self.verified_reports.get(serial)?;
        (self.links.get(mac).is_some_and(|sn| sn == serial)).then_some(mac.as_str())
    }

    pub(crate) fn topology_error(&self) -> Option<&str> {
        self.topology_error.as_deref()
    }

    pub fn resolve(&self, selection: &Selection, time: u64) -> Result<Snapshot, &'static str> {
        let unresolved = "stale or unresolved MQTT target; refresh the device selection";
        let (mac, source) = self
            .automatic_source(&selection.optical_sn, time)?
            .ok_or(unresolved)?;
        let generation = format!(
            "{}-{}-{}-{mac}",
            self.daemon_epoch, self.topology_epoch, source.epoch
        );
        if self.onus.get(&selection.onu_id) != Some(&selection.optical_sn)
            || generation != selection.binding_generation
        {
            return Err(unresolved);
        }
        Ok(Snapshot {
            mqtt_mac: mac.to_owned(),
            optical_sn: Some(selection.optical_sn.clone()),
            onu_id: Some(selection.onu_id),
            binding_generation: generation,
            last_seen: source.last_seen,
            reason: "ready".into(),
        })
    }
}
