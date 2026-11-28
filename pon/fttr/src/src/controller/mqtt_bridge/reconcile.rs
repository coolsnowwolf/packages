use super::super::redirect_net::conntrack;
use super::capture::{ensure_capture, stop_capture, CaptureControl, SynObservation};
use super::evidence::{Lease, MqttSyn, Origin, Transactions};
use super::lan::{
    inspect_lan_context, origin_str, parse_mac_hex12, same_subnet, server_origins, LanContext,
};
use super::nft;
use crate::provisioning::model::Device;
use fttr_api::{DeviceMqttBridgeStatus, MqttBridgeRule, MqttBridgeStatus};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SUBDEV_DIR: &str = "/tmp/fttr_subdev";
pub(super) const SYN_FRESHNESS: Duration = Duration::from_secs(120);
const TRIAL_DURATION: Duration = Duration::from_secs(15);
const TRIAL_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct TrialState {
    mac: [u8; 6],
    started_at: Instant,
}

struct Coordinator {
    enabled: bool,
    seeded_identities: bool,
    broker_ip: Option<Ipv4Addr>,
    lan: LanContext,
    transactions: Transactions,
    leases: BTreeMap<[u8; 6], Lease>,
    syn_attempts: BTreeMap<(String, [u8; 6]), SynObservation>,
    trials: BTreeMap<String, TrialState>,
    failed_trials: BTreeMap<(String, [u8; 6]), Instant>,
    verified_endpoints: BTreeMap<(String, [u8; 6]), (Ipv4Addr, Ipv4Addr, u64)>,
    applied_rules: Vec<nft::RedirectRule>,
    tracked_matchers: BTreeMap<(Ipv4Addr, Ipv4Addr), conntrack::FlowMatcher>,
    cleanup_error: Option<String>,
    rule_error: Option<String>,
    global: MqttBridgeStatus,
    devices: BTreeMap<String, DeviceMqttBridgeStatus>,
    capture: Option<CaptureControl>,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            enabled: false,
            seeded_identities: false,
            broker_ip: None,
            lan: LanContext::default(),
            transactions: Transactions::default(),
            leases: BTreeMap::new(),
            syn_attempts: BTreeMap::new(),
            trials: BTreeMap::new(),
            failed_trials: BTreeMap::new(),
            verified_endpoints: BTreeMap::new(),
            applied_rules: Vec::new(),
            tracked_matchers: BTreeMap::new(),
            cleanup_error: None,
            rule_error: None,
            global: MqttBridgeStatus {
                enabled: false,
                state: "disabled".into(),
                lan_origin: "unknown".into(),
                ..Default::default()
            },
            devices: BTreeMap::new(),
            capture: None,
        }
    }
}

static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();

fn coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| Mutex::new(Coordinator::default()))
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn observe_dhcp_payload(payload: &[u8], source: Ipv4Addr, now_inst: Instant) {
    let wake_needed = if let Ok(mut c) = coordinator().lock() {
        let origins = server_origins(&c.lan);
        if let Some(lease) = c.transactions.observe(payload, source, now_inst, &origins) {
            let changed = c.leases.get(&lease.client) != Some(&lease);
            c.leases.insert(lease.client, lease);
            changed
        } else {
            false
        }
    } else {
        false
    };
    if wake_needed {
        crate::provisioning::controller::wake();
    }
}

pub(super) fn observe_syn(iface: &str, syn: MqttSyn, now_inst: Instant, epoch_secs: u64) {
    let should_wake = if let Ok(mut c) = coordinator().lock() {
        if c.syn_attempts.len() >= 128
            && !c
                .syn_attempts
                .contains_key(&(iface.to_owned(), syn.ethernet_source))
        {
            c.syn_attempts
                .retain(|_, v| now_inst.saturating_duration_since(v.observed_at) <= SYN_FRESHNESS);
            if c.syn_attempts.len() >= 128 {
                return;
            }
        }
        let key = (iface.to_owned(), syn.ethernet_source);
        let is_new_or_changed = c
            .syn_attempts
            .get(&key)
            .is_none_or(|prev| prev.source != syn.source || prev.destination != syn.destination);
        c.syn_attempts.insert(
            key,
            SynObservation {
                source: syn.source,
                destination: syn.destination,
                observed_at: now_inst,
                epoch_secs,
            },
        );
        is_new_or_changed
    } else {
        false
    };
    if should_wake {
        crate::provisioning::controller::wake();
    }
}

fn seed_identities_once(c: &mut Coordinator) {
    if c.seeded_identities {
        return;
    }
    c.seeded_identities = true;
    #[cfg(feature = "device-hl202")]
    if let Ok(entries) = std::fs::read_dir(SUBDEV_DIR) {
        let mut count = 0usize;
        for entry in entries.flatten() {
            if count >= 64 {
                break;
            }
            count += 1;
            let dir_name = entry.file_name();
            let Some(topic_mac) = dir_name.to_str() else {
                continue;
            };
            let bin_path = entry.path().join("telemetry.bin");
            let Ok(meta) = std::fs::metadata(&bin_path) else {
                continue;
            };
            if meta.len() > 64 * 1024 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&bin_path) else {
                continue;
            };
            if let Ok(report) = crate::devices::hl202::telemetry::parse(&bytes) {
                if let Some((serial, mac)) =
                    crate::devices::hl202::telemetry::identity_claim(&report, topic_mac)
                {
                    if let Ok(mut reg) = crate::mqtt_identity::shared().lock() {
                        reg.seed_verified_report(&serial, &mac);
                    }
                }
            }
        }
    }
}

fn cleanup_rules_and_conntrack(c: &mut Coordinator) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(err) = nft::remove() {
        errors.push(err);
    }
    let matchers: Vec<_> = c.tracked_matchers.values().copied().collect();
    let fallback = c.broker_ip.map(|ip| conntrack::FallbackSweep {
        reply_ip: ip,
        reply_port: 1883,
        allowed_orig_ports: &[1883],
    });
    let fallback_slice: &[conntrack::FallbackSweep<'_>] =
        fallback.as_ref().map(std::slice::from_ref).unwrap_or(&[]);
    match conntrack::cleanup_redirected_flows(&matchers, fallback_slice) {
        Ok(_) => c.tracked_matchers.clear(),
        Err(err) => errors.push(err),
    }
    match nft::tables_present() {
        Ok((false, false)) => {}
        Ok((b, i)) => errors.push(format!(
            "nft cleanup incomplete (bridge_present={b}, ip_present={i})"
        )),
        Err(err) => errors.push(err),
    }
    c.applied_rules.clear();
    if errors.is_empty() {
        c.cleanup_error = None;
        Ok(())
    } else {
        let msg = errors.join("; ");
        c.cleanup_error = Some(msg.clone());
        Err(msg)
    }
}

fn is_hl202_candidate(device: &Device) -> bool {
    device.omci_adapter == "hl202"
        || device.provisioning_adapter == "hl202-mqtt"
        || device.bootstrap_adapter == "hl202-mqtt-acs"
        || (device.omci_adapter == "auto" && device.optical_sn.starts_with("H3CT"))
}

pub(crate) fn configure(enabled: bool) {
    let mut c = coordinator().lock().unwrap();
    let changed = c.enabled != enabled;
    c.enabled = enabled;
    if !enabled {
        stop_capture(&mut c.capture);
        c.trials.clear();
        c.failed_trials.clear();
        c.verified_endpoints.clear();
        c.rule_error = None;
        let need_cleanup = !c.applied_rules.is_empty() || !c.tracked_matchers.is_empty() || changed;
        if need_cleanup {
            let res = cleanup_rules_and_conntrack(&mut c);
            for dev in c.devices.values_mut() {
                dev.rule_installed = false;
                dev.mode = "direct".into();
                dev.state = if res.is_err() {
                    "cleanup_failed".into()
                } else {
                    "disabled".into()
                };
                dev.reason = Some(
                    res.as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "Bridge MQTT redirect is disabled".into()),
                );
            }
        }
        c.global.enabled = false;
        c.global.active_rules = 0;
        c.global.rules.clear();
        c.global.state = if c.cleanup_error.is_some() {
            "cleanup_failed".into()
        } else {
            "disabled".into()
        };
        c.global.error = c.cleanup_error.clone();
        c.global.reason = if c.cleanup_error.is_some() {
            c.cleanup_error.clone()
        } else {
            Some("Disabled by configuration".into())
        };
    } else {
        c.cleanup_error = None;
        c.global.enabled = true;
        if c.global.state == "disabled" {
            c.global.state = "waiting".into();
            c.global.reason = Some("Waiting for evidence reconciliation".into());
        }
    }
}

pub(crate) fn set_broker_address(addr: Option<Ipv4Addr>) {
    let mut c = coordinator().lock().unwrap();
    c.broker_ip = addr;
    c.global.broker_listen = addr.map(|ip| format!("{ip}:1883"));
}

pub(crate) fn shutdown() {
    let mut c = coordinator().lock().unwrap();
    c.enabled = false;
    stop_capture(&mut c.capture);
    let _ = cleanup_rules_and_conntrack(&mut c);
}

pub(crate) fn tick(
    online_onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
    configured_devices: &[Device],
) {
    let now_inst = Instant::now();
    let epoch = now_secs();
    let lan = inspect_lan_context();

    let mut c = coordinator().lock().unwrap();
    seed_identities_once(&mut c);
    c.lan = lan.clone();
    c.leases.retain(|_, lease| lease.expires > now_inst);
    c.syn_attempts
        .retain(|_, syn| now_inst.saturating_duration_since(syn.observed_at) <= SYN_FRESHNESS);
    c.failed_trials.retain(|_, until| *until > now_inst);
    c.verified_endpoints
        .retain(|(iface, _), _| interfaces.values().any(|i| i == iface));

    if !c.enabled {
        stop_capture(&mut c.capture);
        if !c.applied_rules.is_empty() || !c.tracked_matchers.is_empty() {
            let _ = cleanup_rules_and_conntrack(&mut c);
        } else if c.cleanup_error.is_some() {
            let _ = cleanup_rules_and_conntrack(&mut c);
        }
        c.global = MqttBridgeStatus {
            enabled: false,
            state: if c.cleanup_error.is_some() {
                "cleanup_failed".into()
            } else {
                "disabled".into()
            },
            lan_origin: origin_str(lan.origin).into(),
            broker_listen: c.broker_ip.map(|ip| format!("{ip}:1883")),
            upstream_gateway: lan.upstream_gateway.map(|ip| ip.to_string()),
            active_rules: 0,
            rules: Vec::new(),
            reason: Some(
                c.cleanup_error
                    .clone()
                    .unwrap_or_else(|| "Disabled by configuration".into()),
            ),
            error: c.cleanup_error.clone(),
        };
        c.devices
            .retain(|sn, _| configured_devices.iter().any(|d| &d.optical_sn == sn));
        let global_state = c.global.state.clone();
        for d in configured_devices {
            c.devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: global_state.clone(),
                    address_origin: origin_str(lan.origin).into(),
                    mode: "direct".into(),
                    rule_installed: false,
                    telemetry_verified: false,
                    reason: Some("Bridge MQTT redirect is disabled".into()),
                    ..Default::default()
                },
            );
        }
        return;
    }

    // Enabled path: determine online HL202 interfaces to capture on.
    let mut capture_targets = BTreeMap::new();
    for (&onu_id, sn) in online_onus {
        let configured_hl202 = configured_devices
            .iter()
            .find(|d| &d.optical_sn == sn)
            .is_some_and(is_hl202_candidate);
        if configured_hl202 || sn.starts_with("H3CT") {
            if let (Ok(id_u8), Some(iface)) = (u8::try_from(onu_id), interfaces.get(&onu_id)) {
                capture_targets.insert(id_u8, iface.clone());
            }
        }
    }
    let enabled = c.enabled;
    ensure_capture(&mut c.capture, enabled, capture_targets);

    let mut desired_rules = Vec::new();
    let mut next_devices = BTreeMap::new();

    for d in configured_devices {
        if !is_hl202_candidate(d) {
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: "not_applicable".into(),
                    address_origin: "unknown".into(),
                    mode: "direct".into(),
                    reason: Some("Device does not use HL202 MQTT".into()),
                    ..Default::default()
                },
            );
            continue;
        }

        let matching_onus: Vec<(u16, String)> = online_onus
            .iter()
            .filter(|(_, sn)| *sn == &d.optical_sn)
            .filter_map(|(id, _)| interfaces.get(id).map(|iface| (*id, iface.clone())))
            .collect();

        if matching_onus.len() != 1 {
            c.trials.remove(&d.optical_sn);
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: "waiting_optical".into(),
                    address_origin: "unknown".into(),
                    mode: "direct".into(),
                    reason: Some(if matching_onus.is_empty() {
                        "Waiting for ONU to reach O5".into()
                    } else {
                        "Multiple online ONUs share this serial number".into()
                    }),
                    ..Default::default()
                },
            );
            continue;
        }
        let (_onu_id, iface) = &matching_onus[0];

        // Snapshot identity & FDB candidates from mqtt_identity.
        let (verified_mac_str, fdb_macs, last_seen) = match crate::mqtt_identity::shared().lock() {
            Ok(reg) => {
                let eval_time = epoch.max(reg.clock());
                let verified = reg
                    .verified_report_candidate(&d.optical_sn, eval_time)
                    .map(str::to_owned);
                let fdb = reg.fdb_candidates_for_serial(&d.optical_sn, eval_time);
                let seen = verified.as_deref().and_then(|m| reg.last_seen(m));
                (verified, fdb, seen)
            }
            Err(_) => (None, Vec::new(), None),
        };

        let telemetry_verified = verified_mac_str.is_some()
            && last_seen
                .is_some_and(|ts| epoch.saturating_sub(ts) <= crate::mqtt_identity::FRESHNESS);

        // Resolve candidate MAC: runtime-verified telemetry identity first, otherwise
        // any FDB MAC on this ONU port actively sending upstream 1883 SYN (15s trial).
        let resolved = {
            let coord = &mut *c;
            resolve_candidate_mac(
                &d.optical_sn,
                iface,
                verified_mac_str.as_deref(),
                &fdb_macs,
                &lan,
                &coord.syn_attempts,
                &mut coord.trials,
                &mut coord.failed_trials,
                now_inst,
            )
        };
        let (mac, binding_source, is_trial) = match resolved {
            CandidateResolution::Selected {
                mac,
                binding_source,
                is_trial,
            } => (mac, Some(binding_source.to_owned()), is_trial),
            CandidateResolution::Cooldown {
                mac,
                syn,
                binding_source,
                reason,
            } => {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "trial_cooldown".into(),
                        address_origin: origin_str(lan.origin).into(),
                        mode: "direct".into(),
                        interface: Some(iface.clone()),
                        client_mac: Some(nft::format_mac(mac)),
                        client_ip: Some(syn.source.to_string()),
                        target_broker: Some(format!("{}:1883", syn.destination)),
                        binding_source: Some(binding_source.into()),
                        reason: Some(reason.into()),
                        ..Default::default()
                    },
                );
                continue;
            }
            CandidateResolution::Waiting { reason } => {
                next_devices.insert(
                    d.optical_sn.clone(),
                    DeviceMqttBridgeStatus {
                        state: "waiting_identity".into(),
                        address_origin: origin_str(lan.origin).into(),
                        mode: "direct".into(),
                        interface: Some(iface.clone()),
                        telemetry_verified,
                        mqtt_last_seen: last_seen,
                        reason: Some(reason.into()),
                        ..Default::default()
                    },
                );
                continue;
            }
        };

        // Establish per-device address origin and target broker.
        let mut address_origin = Origin::Unknown;
        let mut client_ip: Option<Ipv4Addr> = None;
        let mut target_gw: Option<Ipv4Addr> = None;
        let mut evidence_source: Option<String> = None;
        let mut evidence_updated_at: Option<u64> = None;
        let mut wait_reason: Option<String> = None;

        if let Some(&local_ip) = lan.local_leases_by_mac.get(&mac) {
            c.verified_endpoints.remove(&(iface.clone(), mac));
            address_origin = Origin::Local;
            client_ip = Some(local_ip);
            evidence_source = Some("local_dhcp_lease".into());
            evidence_updated_at = Some(epoch);
            wait_reason =
                Some("Device holds a local LAN DHCP lease (NAT mode); redirect disabled".into());
        } else if let Some(lease) = c.leases.get(&mac) {
            address_origin = lease.origin;
            client_ip = Some(lease.address);
            target_gw = lease.gateway.or(lan.upstream_gateway);
            evidence_source = Some("dhcp_ack".into());
            evidence_updated_at = Some(epoch);
            if lease.origin == Origin::Local {
                c.verified_endpoints.remove(&(iface.clone(), mac));
                wait_reason =
                    Some("Observed DHCP ACK from local LAN server; redirect disabled".into());
            } else if lease.origin == Origin::Unknown {
                wait_reason = Some("DHCP server origin is unknown; redirect withheld".into());
            }
        } else if lan.origin == Origin::Upstream {
            if let (Some(lan_ip), Some(gw)) = (lan.lan_ip, lan.upstream_gateway) {
                if let Some(syn) = c.syn_attempts.get(&(iface.clone(), mac)) {
                    if syn.destination == gw
                        && same_subnet(syn.source, lan_ip, lan.lan_mask)
                        && syn.source != lan_ip
                        && !lan.local_lease_ips.contains(&syn.source)
                    {
                        address_origin = Origin::Upstream;
                        client_ip = Some(syn.source);
                        target_gw = Some(gw);
                        evidence_source = Some("upstream_lan_mqtt_attempt".into());
                        evidence_updated_at = Some(syn.epoch_secs);
                    }
                } else if telemetry_verified {
                    if let Some(&(saved_ip, saved_gw, saved_at)) =
                        c.verified_endpoints.get(&(iface.clone(), mac))
                    {
                        if saved_gw == gw
                            && same_subnet(saved_ip, lan_ip, lan.lan_mask)
                            && saved_ip != lan_ip
                            && !lan.local_lease_ips.contains(&saved_ip)
                        {
                            address_origin = Origin::Upstream;
                            client_ip = Some(saved_ip);
                            target_gw = Some(saved_gw);
                            evidence_source = Some("verified_mqtt_session".into());
                            evidence_updated_at = Some(saved_at);
                        }
                    }
                }
            }
            if address_origin != Origin::Upstream {
                wait_reason =
                    Some("Waiting for DHCP ACK or upstream 1883 SYN from this device".into());
            }
        } else {
            wait_reason = lan
                .reason
                .clone()
                .or_else(|| Some("LAN origin is not confirmed as upstream".into()));
        }

        if address_origin == Origin::Local {
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: "local_nat".into(),
                    address_origin: "local".into(),
                    mode: "direct".into(),
                    rule_installed: false,
                    telemetry_verified,
                    interface: Some(iface.clone()),
                    client_mac: Some(nft::format_mac(mac)),
                    client_ip: client_ip.map(|ip| ip.to_string()),
                    target_broker: c.broker_ip.map(|ip| format!("{ip}:1883")),
                    binding_source,
                    evidence_source,
                    evidence_updated_at,
                    mqtt_last_seen: last_seen,
                    reason: wait_reason,
                    ..Default::default()
                },
            );
            continue;
        }

        if address_origin != Origin::Upstream {
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: "waiting_evidence".into(),
                    address_origin: origin_str(address_origin).into(),
                    mode: "direct".into(),
                    rule_installed: false,
                    telemetry_verified,
                    interface: Some(iface.clone()),
                    client_mac: Some(nft::format_mac(mac)),
                    client_ip: client_ip.map(|ip| ip.to_string()),
                    binding_source,
                    evidence_source,
                    evidence_updated_at,
                    mqtt_last_seen: last_seen,
                    reason: wait_reason,
                    ..Default::default()
                },
            );
            continue;
        }

        let (Some(client_ip), Some(target_gw), Some(broker_ip), Some(bridge_mac)) =
            (client_ip, target_gw, c.broker_ip, lan.bridge_mac)
        else {
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: "waiting_broker".into(),
                    address_origin: "upstream".into(),
                    mode: "direct".into(),
                    rule_installed: false,
                    telemetry_verified,
                    interface: Some(iface.clone()),
                    client_mac: Some(nft::format_mac(mac)),
                    client_ip: client_ip.map(|ip| ip.to_string()),
                    target_broker: target_gw.map(|ip| format!("{ip}:1883")),
                    binding_source,
                    evidence_source,
                    evidence_updated_at,
                    mqtt_last_seen: last_seen,
                    reason: Some("Waiting for local MQTT broker and bridge MAC readiness".into()),
                },
            );
            continue;
        };

        if target_gw == broker_ip {
            next_devices.insert(
                d.optical_sn.clone(),
                DeviceMqttBridgeStatus {
                    state: if telemetry_verified {
                        "direct_verified"
                    } else {
                        "direct"
                    }
                    .into(),
                    address_origin: "upstream".into(),
                    mode: "direct".into(),
                    rule_installed: false,
                    telemetry_verified,
                    interface: Some(iface.clone()),
                    client_mac: Some(nft::format_mac(mac)),
                    client_ip: Some(client_ip.to_string()),
                    target_broker: Some(format!("{broker_ip}:1883")),
                    binding_source,
                    evidence_source,
                    evidence_updated_at,
                    mqtt_last_seen: last_seen,
                    reason: Some("Target broker already matches local broker".into()),
                },
            );
            continue;
        }

        if telemetry_verified {
            c.verified_endpoints
                .insert((iface.clone(), mac), (client_ip, target_gw, epoch));
        }
        desired_rules.push(nft::RedirectRule {
            interface: iface.clone(),
            client_mac: mac,
            client_ip,
            target_gw,
            broker_ip,
            bridge_mac,
        });
        c.tracked_matchers.insert(
            (client_ip, target_gw),
            conntrack::FlowMatcher {
                orig_src: Some(client_ip),
                orig_dst: target_gw,
                orig_dst_prefix: 32,
                orig_port: 1883,
                reply_target: (broker_ip, 1883),
            },
        );
        next_devices.insert(
            d.optical_sn.clone(),
            DeviceMqttBridgeStatus {
                state: if is_trial {
                    "trial".into()
                } else if telemetry_verified {
                    "verified".into()
                } else {
                    "redirected".into()
                },
                address_origin: "upstream".into(),
                mode: "redirected".into(),
                rule_installed: true,
                telemetry_verified,
                interface: Some(iface.clone()),
                client_mac: Some(nft::format_mac(mac)),
                client_ip: Some(client_ip.to_string()),
                target_broker: Some(format!("{target_gw}:1883 -> {broker_ip}:1883")),
                binding_source,
                evidence_source,
                evidence_updated_at,
                mqtt_last_seen: last_seen,
                reason: Some(if is_trial {
                    "15s trial redirect active; waiting for matching HL202 telemetry SN".into()
                } else if telemetry_verified {
                    "Redirect active and fresh HL202 telemetry verified".into()
                } else {
                    "Redirect rule installed; waiting for HL202 MQTT reconnect".into()
                }),
            },
        );
    }

    // Reconcile nftables rules atomically when changed or missing.
    let tables_ok = nft::tables_present();
    let needs_sync = match tables_ok {
        Ok((b, i)) => {
            if desired_rules.is_empty() {
                b || i || !c.applied_rules.is_empty()
            } else {
                !b || !i || c.applied_rules != desired_rules
            }
        }
        Err(_) => true,
    };

    if needs_sync {
        if desired_rules.is_empty() {
            if let Err(err) = cleanup_rules_and_conntrack(&mut c) {
                c.rule_error = Some(err);
            } else {
                c.rule_error = None;
            }
        } else {
            // Clean up conntrack for any previously redirected tuples that are no longer desired.
            let desired_keys: BTreeSet<(Ipv4Addr, Ipv4Addr)> = desired_rules
                .iter()
                .map(|r| (r.client_ip, r.target_gw))
                .collect();
            let removed_matchers: Vec<_> = c
                .tracked_matchers
                .iter()
                .filter(|(k, _)| !desired_keys.contains(k))
                .map(|(_, m)| *m)
                .collect();
            match nft::apply(&desired_rules) {
                Ok(()) => {
                    c.applied_rules = desired_rules.clone();
                    c.rule_error = None;
                    if !removed_matchers.is_empty() {
                        if let Err(err) =
                            conntrack::cleanup_redirected_flows(&removed_matchers, &[])
                        {
                            c.rule_error = Some(err);
                        } else {
                            c.tracked_matchers.retain(|k, _| desired_keys.contains(k));
                        }
                    }
                }
                Err(err) => {
                    c.rule_error = Some(err);
                    for dev in next_devices.values_mut() {
                        if dev.rule_installed {
                            dev.rule_installed = false;
                            dev.state = "error".into();
                            dev.reason = c.rule_error.clone();
                        }
                    }
                }
            }
        }
    }

    c.devices = next_devices;
    let active_count = c.devices.values().filter(|d| d.rule_installed).count() as u32;
    let rules = if active_count > 0 {
        build_global_rules(&c.applied_rules, &c.devices, configured_devices)
    } else {
        Vec::new()
    };
    c.global = MqttBridgeStatus {
        enabled: true,
        state: if c.rule_error.is_some() || c.cleanup_error.is_some() {
            "error".into()
        } else if active_count > 0 {
            "active".into()
        } else {
            "waiting".into()
        },
        lan_origin: origin_str(lan.origin).into(),
        broker_listen: c.broker_ip.map(|ip| format!("{ip}:1883")),
        upstream_gateway: lan.upstream_gateway.map(|ip| ip.to_string()),
        active_rules: active_count,
        rules,
        reason: c
            .rule_error
            .clone()
            .or_else(|| c.cleanup_error.clone())
            .or_else(|| lan.reason.clone()),
        error: c.rule_error.clone().or_else(|| c.cleanup_error.clone()),
    };
}

enum CandidateResolution {
    Selected {
        mac: [u8; 6],
        binding_source: &'static str,
        is_trial: bool,
    },
    Cooldown {
        mac: [u8; 6],
        syn: SynObservation,
        binding_source: &'static str,
        reason: &'static str,
    },
    Waiting {
        reason: &'static str,
    },
}

#[allow(clippy::too_many_arguments)]
fn resolve_candidate_mac(
    optical_sn: &str,
    iface: &str,
    verified_mac_str: Option<&str>,
    fdb_macs: &[String],
    lan: &LanContext,
    syn_attempts: &BTreeMap<(String, [u8; 6]), SynObservation>,
    trials: &mut BTreeMap<String, TrialState>,
    failed_trials: &mut BTreeMap<(String, [u8; 6]), Instant>,
    now_inst: Instant,
) -> CandidateResolution {
    if let Some(mac_hex) = verified_mac_str {
        if let Some(mac) = parse_mac_hex12(mac_hex) {
            trials.remove(optical_sn);
            return CandidateResolution::Selected {
                mac,
                binding_source: "verified_report",
                is_trial: false,
            };
        }
    }

    let fdb_set: BTreeSet<[u8; 6]> = fdb_macs
        .iter()
        .filter_map(|hex| parse_mac_hex12(hex))
        .collect();
    if fdb_set.is_empty() {
        return CandidateResolution::Waiting {
            reason: "Waiting for FDB MAC learning on this ONU port",
        };
    }
    if lan.origin != Origin::Upstream {
        return CandidateResolution::Waiting {
            reason: "LAN origin is not confirmed as upstream",
        };
    }
    let (Some(lan_ip), Some(gw)) = (lan.lan_ip, lan.upstream_gateway) else {
        return CandidateResolution::Waiting {
            reason: "Waiting for LAN IPv4 and upstream gateway discovery",
        };
    };

    let mut syn_candidates: Vec<([u8; 6], SynObservation)> = syn_attempts
        .iter()
        .filter(|((syn_iface, mac), syn)| {
            syn_iface == iface
                && fdb_set.contains(mac)
                && syn.destination == gw
                && same_subnet(syn.source, lan_ip, lan.lan_mask)
                && syn.source != lan_ip
                && !lan.local_lease_ips.contains(&syn.source)
                && !lan.local_leases_by_mac.contains_key(mac)
        })
        .map(|((_, mac), syn)| (*mac, *syn))
        .collect();

    if syn_candidates.is_empty() {
        return CandidateResolution::Waiting {
            reason: "Waiting for upstream 1883 SYN from an FDB MAC on this ONU port",
        };
    }

    // Prefer the most recently seen SYN attempt, with deterministic MAC tie-breaking.
    syn_candidates.sort_by(|a, b| b.1.observed_at.cmp(&a.1.observed_at).then(a.0.cmp(&b.0)));

    let mut timed_out: Option<([u8; 6], SynObservation)> = None;
    if let Some(trial) = trials.get(optical_sn).copied() {
        if let Some((_, syn)) = syn_candidates.iter().find(|(mac, _)| *mac == trial.mac) {
            if now_inst.saturating_duration_since(trial.started_at) <= TRIAL_DURATION {
                return CandidateResolution::Selected {
                    mac: trial.mac,
                    binding_source: "trial_syn",
                    is_trial: true,
                };
            }
            failed_trials.insert(
                (optical_sn.to_owned(), trial.mac),
                now_inst + TRIAL_COOLDOWN,
            );
            trials.remove(optical_sn);
            timed_out = Some((trial.mac, *syn));
        } else {
            trials.remove(optical_sn);
        }
    }

    if let Some((mac, _)) = syn_candidates.iter().find(|(mac, _)| {
        failed_trials
            .get(&(optical_sn.to_owned(), *mac))
            .is_none_or(|until| now_inst >= *until)
    }) {
        trials.insert(
            optical_sn.to_owned(),
            TrialState {
                mac: *mac,
                started_at: now_inst,
            },
        );
        return CandidateResolution::Selected {
            mac: *mac,
            binding_source: "trial_syn",
            is_trial: true,
        };
    }

    if let Some((mac, syn)) = timed_out {
        return CandidateResolution::Cooldown {
            mac,
            syn,
            binding_source: "trial_timeout",
            reason: "Trial redirect expired without matching HL202 telemetry SN",
        };
    }

    let (mac, syn) = syn_candidates[0];
    CandidateResolution::Cooldown {
        mac,
        syn,
        binding_source: "trial_cooldown",
        reason: "Previous 15s identity trial did not receive verified telemetry; cooling down",
    }
}

fn build_global_rules(
    applied_rules: &[nft::RedirectRule],
    devices: &BTreeMap<String, DeviceMqttBridgeStatus>,
    configured_devices: &[Device],
) -> Vec<MqttBridgeRule> {
    applied_rules
        .iter()
        .map(|rule| {
            let client_mac = nft::format_mac(rule.client_mac);
            let bridge_mac = nft::format_mac(rule.bridge_mac);
            let matched = devices.iter().find(|(_, status)| {
                status.rule_installed
                    && status.interface.as_deref() == Some(rule.interface.as_str())
                    && status.client_mac.as_deref() == Some(client_mac.as_str())
            });
            let optical_sn = matched.map(|(sn, _)| sn.clone());
            let device = optical_sn.as_ref().and_then(|sn| {
                configured_devices
                    .iter()
                    .find(|d| &d.optical_sn == sn)
                    .map(|d| d.id.clone())
            });
            let status = matched.map(|(_, s)| s);
            MqttBridgeRule {
                interface: rule.interface.clone(),
                client_mac,
                client_ip: rule.client_ip.to_string(),
                target_gw: rule.target_gw.to_string(),
                broker_ip: rule.broker_ip.to_string(),
                bridge_mac,
                device,
                optical_sn,
                state: status.map(|s| s.state.clone()),
                binding_source: status.and_then(|s| s.binding_source.clone()),
                evidence_source: status.and_then(|s| s.evidence_source.clone()),
                bridge_rule: rule.bridge_rule_expr(),
                ip_rule: rule.ip_rule_expr(),
            }
        })
        .collect()
}

pub(crate) fn global_view() -> Value {
    let c = coordinator().lock().unwrap();
    json!(&c.global)
}

pub(crate) fn device_view(optical_sn: &str) -> Value {
    let c = coordinator().lock().unwrap();
    c.devices
        .get(optical_sn)
        .map(|d| json!(d))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn default_coordinator_view_is_disabled_and_unknown() {
        let c = Coordinator::default();
        assert!(!c.global.enabled);
        assert_eq!(c.global.state, "disabled");
        assert_eq!(c.global.lan_origin, "unknown");
    }

    #[test]
    fn build_global_rules_enriches_applied_rules_with_device_metadata_and_nft_expressions() {
        let applied = vec![nft::RedirectRule {
            interface: "fttr127".into(),
            client_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            client_ip: Ipv4Addr::new(192, 0, 2, 244),
            target_gw: Ipv4Addr::new(192, 0, 2, 1),
            broker_ip: Ipv4Addr::new(192, 0, 2, 44),
            bridge_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x02],
        }];
        let mut devices = BTreeMap::new();
        devices.insert(
            "H3CT00000001".into(),
            DeviceMqttBridgeStatus {
                state: "verified".into(),
                address_origin: "upstream".into(),
                mode: "redirected".into(),
                rule_installed: true,
                telemetry_verified: true,
                interface: Some("fttr127".into()),
                client_mac: Some("02:00:00:00:00:01".into()),
                client_ip: Some("192.0.2.244".into()),
                target_broker: Some("192.0.2.1:1883 -> 192.0.2.44:1883".into()),
                binding_source: Some("verified_report".into()),
                evidence_source: Some("dhcp_ack".into()),
                ..Default::default()
            },
        );
        let configured = vec![Device {
            id: "onu_hl202".into(),
            optical_sn: "H3CT00000001".into(),
            cwmp_id: String::new(),
            omci_adapter: "hl202".into(),
            bootstrap_adapter: "none".into(),
            provisioning_adapter: "hl202-mqtt".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            items: Vec::new(),
            wifi_cleanup: String::new(),
            wifi_preserve: BTreeSet::new(),
        }];

        let rules = build_global_rules(&applied, &devices, &configured);
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.device.as_deref(), Some("onu_hl202"));
        assert_eq!(r.optical_sn.as_deref(), Some("H3CT00000001"));
        assert_eq!(r.state.as_deref(), Some("verified"));
        assert_eq!(r.binding_source.as_deref(), Some("verified_report"));
        assert_eq!(r.evidence_source.as_deref(), Some("dhcp_ack"));
        assert_eq!(r.bridge_mac, "02:00:00:00:00:02");
        assert!(r.bridge_rule.contains("iifname \"fttr127\""));
        assert!(r.bridge_rule.contains("ether daddr set 02:00:00:00:00:02"));
        assert!(r.ip_rule.contains("dnat to 192.0.2.44:1883"));
    }

    #[test]
    fn resolve_candidate_mac_allows_multi_mac_fdb_cold_boot_trial_and_cooldown_fallback() {
        let now = Instant::now();
        let lan = LanContext {
            origin: Origin::Upstream,
            lan_ip: Some(Ipv4Addr::new(192, 0, 2, 44)),
            lan_mask: 24,
            upstream_gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
            ..Default::default()
        };
        let fdb_macs = vec![
            "020000000001".into(),
            "020000000002".into(),
            "020000000003".into(),
        ];
        let hl_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let client_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

        let mut syn_attempts = BTreeMap::new();
        syn_attempts.insert(
            ("fttr1".into(), client_mac),
            SynObservation {
                source: Ipv4Addr::new(192, 0, 2, 200),
                destination: Ipv4Addr::new(192, 0, 2, 1),
                observed_at: now,
                epoch_secs: 100,
            },
        );
        syn_attempts.insert(
            ("fttr1".into(), hl_mac),
            SynObservation {
                source: Ipv4Addr::new(192, 0, 2, 244),
                destination: Ipv4Addr::new(192, 0, 2, 1),
                observed_at: now - Duration::from_secs(1),
                epoch_secs: 99,
            },
        );
        // Non-FDB MAC must be ignored even with a newer SYN.
        syn_attempts.insert(
            ("fttr1".into(), [0x02, 0x00, 0x00, 0x00, 0x00, 0x99]),
            SynObservation {
                source: Ipv4Addr::new(192, 0, 2, 201),
                destination: Ipv4Addr::new(192, 0, 2, 1),
                observed_at: now + Duration::from_secs(1),
                epoch_secs: 101,
            },
        );

        let mut trials = BTreeMap::new();
        let mut failed_trials = BTreeMap::new();

        // Cold boot (no verified_report) with 3 FDB MACs starts trial on most recent SYN.
        match resolve_candidate_mac(
            "H3CT00000001",
            "fttr1",
            None,
            &fdb_macs,
            &lan,
            &syn_attempts,
            &mut trials,
            &mut failed_trials,
            now,
        ) {
            CandidateResolution::Selected {
                mac,
                binding_source,
                is_trial,
            } => {
                assert_eq!(mac, client_mac);
                assert_eq!(binding_source, "trial_syn");
                assert!(is_trial);
            }
            _ => panic!("expected trial selection"),
        }

        // After 16s without telemetry, client_mac enters cooldown and next candidate hl_mac is tried.
        let t_timeout = now + Duration::from_secs(16);
        match resolve_candidate_mac(
            "H3CT00000001",
            "fttr1",
            None,
            &fdb_macs,
            &lan,
            &syn_attempts,
            &mut trials,
            &mut failed_trials,
            t_timeout,
        ) {
            CandidateResolution::Selected {
                mac,
                binding_source,
                is_trial,
            } => {
                assert_eq!(mac, hl_mac);
                assert_eq!(binding_source, "trial_syn");
                assert!(is_trial);
            }
            _ => panic!("expected fallback to second candidate MAC"),
        }

        // After hl_mac also times out, all candidates are in cooldown.
        let t_all_cooled = t_timeout + Duration::from_secs(16);
        match resolve_candidate_mac(
            "H3CT00000001",
            "fttr1",
            None,
            &fdb_macs,
            &lan,
            &syn_attempts,
            &mut trials,
            &mut failed_trials,
            t_all_cooled,
        ) {
            CandidateResolution::Cooldown {
                mac,
                binding_source,
                ..
            } => {
                assert_eq!(mac, hl_mac);
                assert_eq!(binding_source, "trial_timeout");
            }
            _ => panic!("expected cooldown once all candidates expired"),
        }
    }
}
