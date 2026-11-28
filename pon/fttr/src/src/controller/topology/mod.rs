//! Bounded, read-only topology snapshots. Never performs OMCI/CWMP writes or probes.
use fttr_api::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(feature = "device-hl202")]
mod hl202;
mod local;
#[cfg(test)]
mod tests;

const TTL: u32 = 30;
const CACHE_SIZE: usize = 4;
const CONTROLLER: &str = "controller";

pub(super) fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn normalize_mac(value: &str) -> Option<String> {
    let hex: String = value.chars().filter(|c| !matches!(c, ':' | '-')).collect();
    if hex.len() != 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<_> = (0..6)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    if bytes[0] & 1 != 0 || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn serial(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    (!value.is_empty()).then_some(value)
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("unknown")).then(|| value.into())
}

pub(crate) struct Snapshot {
    pub reply: TopologyReply,
    at: Instant,
    clients: BTreeMap<String, Vec<TopologyClient>>,
    associated_count: Option<u32>,
    learned_available: BTreeSet<String>,
    client_sources: Vec<TopologySource>,
}

#[derive(Default)]
pub(crate) struct Cache(VecDeque<std::sync::Arc<Snapshot>>);

impl Cache {
    pub fn insert(&mut self, snapshot: Snapshot) -> std::sync::Arc<Snapshot> {
        self.0
            .retain(|s| s.at.elapsed() < Duration::from_secs(u64::from(TTL)));
        while self.0.len() >= CACHE_SIZE {
            self.0.pop_front();
        }
        let snapshot = std::sync::Arc::new(snapshot);
        self.0.push_back(snapshot.clone());
        snapshot
    }

    pub fn get(&mut self, id: &str) -> Option<std::sync::Arc<Snapshot>> {
        self.0
            .retain(|s| s.at.elapsed() < Duration::from_secs(u64::from(TTL)));
        self.0.iter().find(|s| s.reply.snapshot_id == id).cloned()
    }
}

impl Snapshot {
    fn client_cursor(&self, node_id: &str, offset: usize) -> String {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.reply.snapshot_id.hash(&mut hash);
        node_id.hash(&mut hash);
        offset.hash(&mut hash);
        format!("{:016x}:{offset}", hash.finish())
    }

    pub fn node(&self, id: &str) -> TopologyNodeReply {
        match self.reply.nodes.iter().find(|n| n.id == id) {
            Some(node) => TopologyNodeReply::Ok {
                snapshot_id: self.reply.snapshot_id.clone(),
                node: node.clone(),
            },
            None => TopologyNodeReply::Error {
                code: TopologyQueryError::NodeNotFound,
            },
        }
    }

    pub fn clients(&self, request: &TopologyClientsRequest) -> TopologyClientsReply {
        if !self.reply.nodes.iter().any(|n| n.id == request.node_id) {
            return TopologyClientsReply::Error {
                code: TopologyQueryError::NodeNotFound,
            };
        }
        let entries = self
            .clients
            .get(&request.node_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let start = match request.cursor.as_deref() {
            None => 0,
            Some(cursor) => match cursor
                .split_once(':')
                .and_then(|(_, s)| s.parse::<usize>().ok())
            {
                Some(offset)
                    if request.snapshot_id.as_deref() == Some(&self.reply.snapshot_id)
                        && offset > 0
                        && offset < entries.len()
                        && cursor == self.client_cursor(&request.node_id, offset) =>
                {
                    offset
                }
                _ => {
                    return TopologyClientsReply::Error {
                        code: TopologyQueryError::InvalidCursor,
                    }
                }
            },
        };
        let end =
            (start + usize::from(request.limit.unwrap_or(100).clamp(1, 256))).min(entries.len());
        let associated = if request.node_id == CONTROLLER {
            self.associated_count
        } else {
            None
        };
        let operational = self.reply.nodes.iter().find(|n| n.id == request.node_id).is_some_and(|n| {
            matches!(&n.detail, TopologyNodeDetail::Onu(onu) if onu.registration == Some(RegistrationState::O5Operation))
        });
        TopologyClientsReply::Ok {
            snapshot_id: self.reply.snapshot_id.clone(),
            parent_node_id: request.node_id.clone(),
            clients: entries[start..end].to_vec(),
            associated_client_count: associated,
            learned_host_count: (self.learned_available.contains(&request.node_id) && operational)
                .then(|| {
                    entries
                        .iter()
                        .filter(|c| c.relation == TopologyRelation::LearnedBehind)
                        .count() as u32
                }),
            complete: false,
            next_cursor: (end < entries.len()).then(|| self.client_cursor(&request.node_id, end)),
            sources: self.client_sources.clone(),
        }
    }
}

pub(crate) fn build(
    status: StatusReply,
    engine: &std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: &super::service::States,
) -> Snapshot {
    let sampled_at = unix_time();
    let local = local::collect();
    #[cfg(feature = "device-hl202")]
    let extensions = hl202::collect(engine, services);
    #[cfg(not(feature = "device-hl202"))]
    let extensions: BTreeMap<String, TopologyHl202> = {
        let _ = (engine, services);
        BTreeMap::new()
    };
    project(status, local, extensions, sampled_at)
}

fn binding(sn: Option<&str>, pins: Option<&OnuPinning>) -> TopologyBinding {
    let lookup = |map: &BTreeMap<String, u8>| {
        sn.and_then(|sn| {
            map.iter()
                .find(|(key, _)| serial(key).as_deref() == Some(sn))
                .map(|(_, id)| *id)
        })
    };
    let active = pins.and_then(|p| lookup(&p.active));
    let desired = pins.and_then(|p| lookup(&p.desired));
    let state = match (active, desired) {
        (None, None) => TopologyBindingState::None,
        (Some(a), Some(b)) if a == b => TopologyBindingState::Effective,
        (None, Some(_)) => TopologyBindingState::PendingAdd,
        (Some(_), None) => TopologyBindingState::PendingRemove,
        _ => TopologyBindingState::PendingChange,
    };
    TopologyBinding {
        active_onu_id: active,
        desired_onu_id: desired,
        state,
        pending_restart: pins.is_some_and(|p| p.pending_restart),
    }
}

fn project(
    status: StatusReply,
    local: local::Local,
    mut extensions: BTreeMap<String, TopologyHl202>,
    sampled_at: u64,
) -> Snapshot {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let at = Instant::now();
    let now = unix_time();
    let mut issues = Vec::new();
    let mut sources = local.sources.clone();
    sources.push(TopologySource {
        id: "fttr.status".into(),
        state: TopologySourceState::Ok,
        sampled_at,
        reason: None,
    });
    let module = status.optical_transceiver;
    let mut nodes = vec![TopologyNode {
        id: CONTROLLER.into(),
        label: local
            .model
            .clone()
            .unwrap_or_else(|| "FTTR controller".into()),
        detail: TopologyNodeDetail::Controller(TopologyController {
            model: local.model,
            firmware_version: local.firmware,
            uptime_seconds: local.uptime,
            fpga_status: status.fpga_status,
            optical: TopologyOpticalModule {
                model: module.model,
                wavelength_tx_nm: module.wavelength_tx_nm,
                wavelength_rx_nm: module.wavelength_rx_nm,
                nominal_downlink_gbps: module.phy_rate_downlink_gbps,
                nominal_uplink_gbps: module.phy_rate_uplink_gbps,
                tx_power_dbm: module.tx_power_dbm,
                laser_bias_current_ma: module.laser_bias_current_ma,
                temperature_celsius: module.temperature_celsius,
                vcc_voltage: module.vcc_voltage,
                tx_enabled: module.tx_enabled,
                measurements: module.measurements,
            },
            interfaces: local.interfaces.clone(),
            uplinks: local.uplinks,
        }),
    }];
    let mut counts = BTreeMap::<String, usize>::new();
    for onu in &status.sub_gateways {
        if let Some(sn) = serial(&onu.serial_number) {
            *counts.entry(sn).or_default() += 1;
        }
    }
    let mut links = Vec::new();
    let mut learned_onus = BTreeMap::new();
    let mut onu_interfaces = BTreeMap::new();
    let mut node_by_sn = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for onu in status.sub_gateways {
        let sn = serial(&onu.serial_number);
        let unique = sn.as_ref().is_some_and(|sn| counts.get(sn) == Some(&1));
        let id = match &sn {
            Some(sn) if unique => format!("onu:{sn}"),
            Some(sn) => format!("onu:{sn}#{}", onu.onu_id),
            None => format!("onu:slot:{}", onu.onu_id),
        };
        if !unique {
            issues.push(TopologyIssue {
                code: if sn.is_some() {
                    "duplicate_serial"
                } else {
                    "missing_serial"
                }
                .into(),
                node_id: Some(id.clone()),
            });
        }
        if !seen.insert(id.clone()) {
            issues.push(TopologyIssue {
                code: "duplicate_registration".into(),
                node_id: Some(id),
            });
            continue;
        }
        let pin = binding(sn.as_deref(), status.onu_pinning.as_ref());
        let interface = onu.data_path.interface.clone();
        let shared_interface = onu
            .data_path
            .measurements
            .get("interface_scope")
            .is_some_and(|scope| scope.raw.as_deref() == Some("shared_nni"));
        // Prefer the direct read; preserve unknown on read error rather than false.
        let carrier = (!shared_interface)
            .then(|| {
                read_carrier(&interface).or_else(|| {
                    local
                        .interfaces
                        .iter()
                        .find(|i| i.name == interface)
                        .and_then(|i| i.carrier)
                })
            })
            .flatten();
        let operational = onu.state == RegistrationState::O5Operation;
        if unique && operational {
            learned_onus.insert(u16::from(onu.onu_id), sn.clone().unwrap());
            if !shared_interface {
                onu_interfaces.insert(u16::from(onu.onu_id), interface.clone());
            }
            node_by_sn.insert(sn.clone().unwrap(), id.clone());
        }
        let ext = if unique && operational && onu.identity_current {
            sn.as_ref()
                .and_then(|sn| extensions.remove(sn))
                .filter(|ext| {
                    // A report predating this registration's OMCI identity is historical.
                    onu.identity_age_seconds
                        .is_some_and(|age| ext.age_seconds <= age)
                })
        } else {
            None
        };
        if let Some(extension) = &ext {
            sources.push(TopologySource {
                id: format!("hl202.report:{id}"),
                state: TopologySourceState::Ok,
                sampled_at: now.saturating_sub(extension.age_seconds),
                reason: None,
            });
        }
        links.push(TopologyLink {
            id: format!("pon:{id}"),
            source: CONTROLLER.into(),
            target: id.clone(),
            relation: TopologyRelation::PonLogical,
            data_interface: Some(interface),
            carrier,
            onu_rx_power_dbm: onu.optical_rx_power_dbm,
            onu_tx_power_dbm: onu.optical_tx_power_dbm,
            equalization_delay_bits: operational.then_some(onu.equalization_delay_bits).flatten(),
            data_path: Some(TopologyDataPath {
                observer_node_id: CONTROLLER.into(),
                rx_bytes: onu.data_path.rx_bytes,
                tx_bytes: onu.data_path.tx_bytes,
                current_rx_kbps: onu.data_path.current_rx_kbps,
                current_tx_kbps: onu.data_path.current_tx_kbps,
                onu_inventory: onu.data_path.onu_inventory,
                offload: onu.data_path.offload,
                measurements: onu
                    .data_path
                    .measurements
                    .into_iter()
                    .filter(|(k, _)| k != "vlan_id" && k != "forwarding_proof")
                    .collect(),
            }),
            measurements: onu
                .measurements
                .iter()
                .filter(|(k, _)| {
                    matches!(
                        k.as_str(),
                        "optical_rx_power_dbm" | "optical_tx_power_dbm" | "equalization_delay_bits"
                    )
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        });
        nodes.push(TopologyNode {
            id,
            label: nonempty(&onu.model)
                .or_else(|| sn.clone())
                .unwrap_or_else(|| format!("ONU {}", onu.onu_id)),
            detail: TopologyNodeDetail::Onu(TopologyOnu {
                serial_number: sn,
                onu_id: Some(onu.onu_id),
                vendor: nonempty(&onu.vendor),
                model: nonempty(&onu.model),
                hardware_version: nonempty(&onu.hardware_version),
                firmware_version: nonempty(&onu.firmware_version),
                registration: Some(onu.state),
                pon_mode: onu.pon_mode,
                upstream_phy_rate_gbps: onu.upstream_phy_rate_gbps,
                business: Some(onu.business_status),
                management: onu.management,
                provisioning: onu.provisioning,
                services: onu.services,
                ssids: onu.ssids,
                wifi_sync: Some(onu.wifi_sync),
                identity: onu.identity,
                identity_current: onu.identity_current,
                identity_age_seconds: onu.identity_age_seconds,
                uptime_seconds: onu.uptime_seconds,
                fec: onu.fec,
                binding: pin,
                extensions: TopologyExtensions { hl202: ext },
                measurements: onu
                    .measurements
                    .into_iter()
                    .filter(|(k, _)| k == "uptime_seconds")
                    .collect(),
            }),
        });
    }
    if let Some(pins) = &status.onu_pinning {
        let reserved: BTreeSet<_> = pins
            .active
            .keys()
            .chain(pins.desired.keys())
            .filter_map(|s| serial(s))
            .collect();
        for sn in reserved {
            if counts.contains_key(&sn) {
                continue;
            }
            let id = format!("onu:{sn}");
            let pin = binding(Some(&sn), Some(pins));
            nodes.push(TopologyNode {
                id: id.clone(),
                label: sn.clone(),
                detail: TopologyNodeDetail::Onu(TopologyOnu {
                    serial_number: Some(sn),
                    onu_id: None,
                    vendor: None,
                    model: None,
                    hardware_version: None,
                    firmware_version: None,
                    registration: None,
                    pon_mode: None,
                    upstream_phy_rate_gbps: None,
                    business: None,
                    management: None,
                    provisioning: None,
                    services: Vec::new(),
                    ssids: Vec::new(),
                    wifi_sync: None,
                    identity: None,
                    identity_current: false,
                    identity_age_seconds: None,
                    uptime_seconds: None,
                    fec: None,
                    binding: pin,
                    extensions: TopologyExtensions::default(),
                    measurements: BTreeMap::new(),
                }),
            });
            links.push(TopologyLink {
                id: format!("pon:{id}"),
                source: CONTROLLER.into(),
                target: id,
                relation: TopologyRelation::PonLogical,
                data_interface: None,
                carrier: None,
                onu_rx_power_dbm: None,
                onu_tx_power_dbm: None,
                equalization_delay_bits: None,
                data_path: None,
                measurements: BTreeMap::new(),
            });
        }
        if pins.allocation_blocked || pins.config_error.is_some() || pins.startup_error.is_some() {
            issues.push(TopologyIssue {
                code: "pinning_error".into(),
                node_id: None,
            });
        }
    }
    let mut occupied = BTreeMap::<String, Vec<String>>::new();
    for link in &links {
        if link
            .data_path
            .as_ref()
            .and_then(|path| path.measurements.get("interface_scope"))
            .is_some_and(|scope| scope.raw.as_deref() == Some("shared_nni"))
        {
            continue;
        }
        if let Some(interface) = &link.data_interface {
            occupied
                .entry(interface.clone())
                .or_default()
                .push(link.target.clone());
        }
    }
    for ids in occupied.values().filter(|ids| ids.len() > 1) {
        for id in ids {
            issues.push(TopologyIssue {
                code: "port_conflict".into(),
                node_id: Some(id.clone()),
            });
        }
    }
    let (clients, associated_count, learned_available, client_sources) = collect_clients(
        &onu_interfaces,
        &nodes,
        &learned_onus,
        &node_by_sn,
        &local.interfaces,
        local.hints,
        local.associated,
        local.association_complete,
        local.sources,
    );
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Snapshot {
        reply: TopologyReply {
            schema_version: 1,
            snapshot_id: format!("{stamp:x}-{:x}", SEQUENCE.fetch_add(1, Ordering::Relaxed)),
            generated_at: now,
            snapshot_ttl_seconds: TTL,
            nodes,
            links,
            sources,
            issues,
        },
        at,
        clients,
        associated_count,
        learned_available,
        client_sources,
    }
}

fn read_carrier(interface: &str) -> Option<bool> {
    if !local::valid_interface(interface) {
        return None;
    }
    match std::fs::read_to_string(format!("/sys/class/net/{interface}/carrier"))
        .ok()?
        .trim()
    {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_clients(
    onu_interfaces: &BTreeMap<u16, String>,
    nodes: &[TopologyNode],
    onus: &BTreeMap<u16, String>,
    node_by_sn: &BTreeMap<String, String>,
    interfaces: &[TopologyInterface],
    hints: BTreeMap<String, (Option<String>, Vec<String>)>,
    associated: Vec<TopologyClient>,
    association_complete: bool,
    mut sources: Vec<TopologySource>,
) -> (
    BTreeMap<String, Vec<TopologyClient>>,
    Option<u32>,
    BTreeSet<String>,
    Vec<TopologySource>,
) {
    let mut clients = BTreeMap::<String, Vec<TopologyClient>>::new();
    let mut excluded: BTreeSet<String> = interfaces.iter().filter_map(|i| i.mac.clone()).collect();
    for node in nodes {
        if let TopologyNodeDetail::Onu(onu) = &node.detail {
            if let Some(mac) = onu.extensions.hl202.as_ref().and_then(|h| h.mac.clone()) {
                excluded.insert(mac);
            }
        }
    }
    let eligible: BTreeSet<String> = onus
        .iter()
        .filter_map(|(id, sn)| {
            let interface = onu_interfaces.get(id)?;
            std::path::Path::new(&format!("/sys/class/net/{}/brport/port_no", interface))
                .exists()
                .then(|| node_by_sn.get(sn).cloned())
                .flatten()
        })
        .collect();
    let learned = if !eligible.is_empty() {
        crate::provisioning::link_identity::discover(onus, onu_interfaces).ok()
    } else {
        None
    };
    let learned_available = if learned.is_some() {
        eligible
    } else {
        BTreeSet::new()
    };
    sources.push(TopologySource {
        id: "bridge.fdb".into(),
        state: if !learned_available.is_empty() {
            TopologySourceState::Partial
        } else {
            TopologySourceState::Unavailable
        },
        sampled_at: unix_time(),
        reason: Some(
            if !learned_available.is_empty() {
                "learned_sources_not_exhaustive_terminal_inventory"
            } else {
                "bridge_fdb_unavailable"
            }
            .into(),
        ),
    });
    for (mac, sn) in learned.unwrap_or_default() {
        if let (Some(mac), Some(parent)) = (normalize_mac(&mac), node_by_sn.get(&sn)) {
            if excluded.contains(&mac) {
                continue;
            }
            clients
                .entry(parent.clone())
                .or_default()
                .push(TopologyClient {
                    id: format!("host:{mac}"),
                    mac,
                    hostname: None,
                    addresses: Vec::new(),
                    relation: TopologyRelation::LearnedBehind,
                    source: "bridge.fdb".into(),
                    interface: onus
                        .iter()
                        .find(|(_, serial)| **serial == sn)
                        .and_then(|(id, _)| onu_interfaces.get(id).cloned()),
                });
        }
    }
    let associated_count = association_complete.then(|| {
        associated
            .iter()
            .map(|c| c.mac.as_str())
            .collect::<BTreeSet<_>>()
            .len() as u32
    });
    clients.insert(CONTROLLER.into(), associated);
    for entries in clients.values_mut() {
        for entry in entries.iter_mut() {
            if let Some((name, addresses)) = hints.get(&entry.mac) {
                entry.hostname = name.clone();
                entry.addresses = addresses.clone();
            }
        }
        entries.sort_by(|a, b| a.mac.cmp(&b.mac).then(a.interface.cmp(&b.interface)));
        entries.dedup_by(|a, b| a.mac == b.mac && a.interface == b.interface);
    }
    (clients, associated_count, learned_available, sources)
}
