use super::super::redirect_net::{
    conntrack,
    lan::{
        inspect_lan_context, origin_str, parse_mac_hex12, read_subnet_leases, same_subnet,
        LanContext,
    },
    nft as net_nft,
};
use super::nft;
use crate::acs::PathPolicy;
use crate::provisioning::management_network::Desired;
use crate::provisioning::model::{
    resolve_configured_device, AcsBridgeConfig, AcsPathMatch, AcsRedirectRuleConfig, Device,
};
use fttr_api::{AcsBridgeRule, AcsBridgeStatus};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_ACS_PORT: u16 = 7547;
const DEFAULT_ACS_PATH: &str = "/cwmp";

type MatcherKey = (Option<Ipv4Addr>, Ipv4Addr, u8, u16, Ipv4Addr, u16);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct Coordinator {
    config: AcsBridgeConfig,
    lan: LanContext,
    applied_bridge_rules: Vec<String>,
    applied_ip_rules: Vec<String>,
    tracked_matchers: BTreeMap<MatcherKey, conntrack::FlowMatcher>,
    known_targets: Vec<(Ipv4Addr, u16)>,
    known_orig_ports: BTreeSet<u16>,
    cleanup_pending: bool,
    cleanup_error: Option<String>,
    rule_error: Option<String>,
    listener_error: Option<String>,
    status: AcsBridgeStatus,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            config: AcsBridgeConfig::default(),
            lan: LanContext::default(),
            applied_bridge_rules: Vec::new(),
            applied_ip_rules: Vec::new(),
            tracked_matchers: BTreeMap::new(),
            known_targets: Vec::new(),
            known_orig_ports: BTreeSet::from([DEFAULT_ACS_PORT]),
            // Inspect nftables on the first tick so orphaned tables from a prior crash are cleaned up.
            cleanup_pending: true,
            cleanup_error: None,
            rule_error: None,
            listener_error: None,
            status: AcsBridgeStatus {
                enabled: false,
                management_gateway: false,
                state: "disabled".into(),
                lan_origin: "unknown".into(),
                path_mode: "none".into(),
                ..Default::default()
            },
        }
    }
}

static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();

fn coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| Mutex::new(Coordinator::default()))
}

struct PlannedRule {
    config: AcsRedirectRuleConfig,
    view: AcsBridgeRule,
    lan_bridge_rules: Vec<String>,
    lan_ip_rules: Vec<String>,
    mgmt_ip_rules: Vec<String>,
    matchers: Vec<(MatcherKey, conntrack::FlowMatcher)>,
}

struct Plan {
    lan_target_addr: Option<(Ipv4Addr, u16)>,
    mgmt_target_addr: Option<(Ipv4Addr, u16)>,
    bridge_rules: Vec<String>,
    ip_rules: Vec<String>,
    desired_matchers: BTreeMap<MatcherKey, conntrack::FlowMatcher>,
    orig_ports: BTreeSet<u16>,
    rules: Vec<PlannedRule>,
    primary_path: String,
}

fn matcher_key(m: &conntrack::FlowMatcher) -> MatcherKey {
    (
        m.orig_src,
        m.orig_dst,
        m.orig_dst_prefix,
        m.orig_port,
        m.reply_target.0,
        m.reply_target.1,
    )
}

fn build_listener_path_policy(
    rules: &[&AcsRedirectRuleConfig],
    primary_path: &str,
) -> (PathPolicy, String, Vec<String>) {
    let mut allow_any = false;
    let mut has_exact = false;
    let mut exact_extra = BTreeSet::new();
    let mut accepted = BTreeSet::new();
    accepted.insert(primary_path.to_owned());

    for rule in rules {
        match &rule.path {
            AcsPathMatch::Wildcard => allow_any = true,
            AcsPathMatch::Exact(path) => {
                has_exact = true;
                accepted.insert(path.clone());
                if path != primary_path {
                    exact_extra.insert(path.clone());
                }
            }
        }
    }

    let mode = if allow_any {
        "wildcard".to_owned()
    } else if has_exact {
        "exact".to_owned()
    } else {
        "none".to_owned()
    };

    let mut accepted_paths = Vec::new();
    if allow_any {
        accepted_paths.push("*".into());
    }
    accepted_paths.extend(accepted);
    (
        PathPolicy {
            allow_any,
            extra_paths: exact_extra.into_iter().collect(),
        },
        mode,
        accepted_paths,
    )
}

fn resolve_device_onus(
    device_target: Option<&str>,
    online_onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
    configured_devices: &[Device],
) -> Result<(Option<Device>, Vec<String>), String> {
    if let Some(target) = device_target {
        let dev = resolve_configured_device(configured_devices, target)?;
        let mut matched_ifaces = Vec::new();
        for (onu_id, sn) in online_onus {
            if sn.eq_ignore_ascii_case(&dev.optical_sn) {
                if let Some(iface) = interfaces.get(onu_id) {
                    if net_nft::valid_interface(iface) {
                        matched_ifaces.push(iface.clone());
                    }
                }
            }
        }
        matched_ifaces.sort();
        matched_ifaces.dedup();
        match matched_ifaces.len() {
            0 => Err(format!("target FTTR ONU '{}' is offline", dev.id)),
            1 => Ok((Some(dev.clone()), matched_ifaces)),
            _ => Err(format!(
                "ambiguous online interfaces for FTTR ONU '{}': {}",
                dev.id,
                matched_ifaces.join(", ")
            )),
        }
    } else {
        let mut all_ifaces: Vec<String> = online_onus
            .keys()
            .filter_map(|onu_id| interfaces.get(onu_id))
            .filter(|iface| net_nft::valid_interface(iface))
            .cloned()
            .collect();
        all_ifaces.sort();
        all_ifaces.dedup();
        if all_ifaces.is_empty() {
            return Err("waiting for an online FTTR ONU".into());
        }
        Ok((None, all_ifaces))
    }
}

fn collect_device_mgmt_candidates(
    dev: &Device,
    mgmt_ip: Ipv4Addr,
    mgmt_prefix: u8,
    mgmt_leases: &BTreeMap<[u8; 6], Ipv4Addr>,
    source_links: Option<&BTreeMap<String, String>>,
    fdb_macs_for_serial: &BTreeMap<String, Vec<String>>,
    acs_devices: &crate::cwmp::observations::DeviceObservations,
) -> BTreeSet<Ipv4Addr> {
    let mut candidates = BTreeSet::new();
    for entry in acs_devices.values() {
        if crate::devices::matches_cwmp_identity(
            &dev.optical_sn,
            &entry.product_class,
            &entry.serial,
        ) {
            if let Some(ip) = entry
                .peer
                .filter(|&ip| ip != mgmt_ip && same_subnet(ip, mgmt_ip, mgmt_prefix))
            {
                candidates.insert(ip);
            }
        }
    }

    let mut owned_macs = BTreeSet::new();
    if let Some(links) = source_links {
        for (mac_hex, serial) in links {
            if serial.eq_ignore_ascii_case(&dev.optical_sn) {
                if let Some(mac) = parse_mac_hex12(mac_hex) {
                    owned_macs.insert(mac);
                }
            }
        }
    }
    for (serial_key, macs) in fdb_macs_for_serial {
        if serial_key.eq_ignore_ascii_case(&dev.optical_sn) {
            for mac_hex in macs {
                let claimed_elsewhere = source_links.is_some_and(|links| {
                    links.iter().any(|(other_mac, other_sn)| {
                        other_mac.eq_ignore_ascii_case(mac_hex)
                            && !other_sn.eq_ignore_ascii_case(&dev.optical_sn)
                    })
                });
                if !claimed_elsewhere {
                    if let Some(mac) = parse_mac_hex12(mac_hex) {
                        owned_macs.insert(mac);
                    }
                }
            }
        }
    }
    for mac in owned_macs {
        if let Some(&ip) = mgmt_leases.get(&mac) {
            if ip != mgmt_ip && same_subnet(ip, mgmt_ip, mgmt_prefix) {
                candidates.insert(ip);
            }
        }
    }
    candidates
}

#[allow(clippy::too_many_arguments)]
fn resolve_management_source_ip(
    dev: &Device,
    mgmt_ip: Ipv4Addr,
    mgmt_prefix: u8,
    mgmt_leases: &BTreeMap<[u8; 6], Ipv4Addr>,
    source_links: Option<&BTreeMap<String, String>>,
    fdb_macs_for_serial: &BTreeMap<String, Vec<String>>,
    acs_devices: &crate::cwmp::observations::DeviceObservations,
    configured_devices: &[Device],
) -> Result<Ipv4Addr, String> {
    let candidates = collect_device_mgmt_candidates(
        dev,
        mgmt_ip,
        mgmt_prefix,
        mgmt_leases,
        source_links,
        fdb_macs_for_serial,
        acs_devices,
    );
    if candidates.is_empty() {
        return Err(format!(
            "waiting for unique management IPv4 of FTTR ONU '{}' on br-tr069",
            dev.id
        ));
    }
    if candidates.len() > 1 {
        return Err(format!(
            "ambiguous management IPv4 candidates on br-tr069 for FTTR ONU '{}'",
            dev.id
        ));
    }
    let ip = *candidates.iter().next().unwrap();
    for other in configured_devices {
        if other.optical_sn.eq_ignore_ascii_case(&dev.optical_sn) {
            continue;
        }
        let other_ips = collect_device_mgmt_candidates(
            other,
            mgmt_ip,
            mgmt_prefix,
            mgmt_leases,
            source_links,
            fdb_macs_for_serial,
            acs_devices,
        );
        if other_ips.contains(&ip) {
            return Err(format!(
                "management IPv4 {ip} is claimed by multiple FTTR ONUs on br-tr069"
            ));
        }
    }
    Ok(ip)
}

#[allow(clippy::too_many_arguments)]
fn build_plan(
    config: &AcsBridgeConfig,
    lan: &LanContext,
    online_onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
    configured_devices: &[Device],
    management_network: Option<&Desired>,
    management_active: bool,
    mgmt_leases: &BTreeMap<[u8; 6], Ipv4Addr>,
    source_links: Option<&BTreeMap<String, String>>,
    fdb_macs_for_serial: &BTreeMap<String, Vec<String>>,
    acs_devices: &crate::cwmp::observations::DeviceObservations,
    listener_error: Option<&str>,
) -> Plan {
    let acs_port = management_network
        .map(|m| m.port)
        .unwrap_or(DEFAULT_ACS_PORT);
    let primary_path = management_network
        .map(|m| m.path.as_str())
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_ACS_PATH)
        .to_owned();

    let lan_target_addr = lan.lan_ip.map(|ip| (ip, acs_port));
    let mgmt_target_addr = management_network
        .filter(|m| management_active && m.enabled && m.acs_enabled)
        .and_then(|m| {
            m.address
                .parse::<Ipv4Addr>()
                .ok()
                .map(|ip| (ip, m.prefix, m.port))
        });

    let mut bridge_rules_set = BTreeSet::new();
    let mut ip_rules_set = BTreeSet::new();
    let mut desired_matchers = BTreeMap::new();
    let mut orig_ports = BTreeSet::new();
    let mut planned_rules = Vec::with_capacity(config.rules.len());

    for rule in &config.rules {
        let target_label = nft::target_expr(rule.target_ip, rule.target_prefix);
        let (path_val, rule_path_mode) = match &rule.path {
            AcsPathMatch::Wildcard => (None, "wildcard".to_owned()),
            AcsPathMatch::Exact(p) => (Some(p.clone()), "exact".to_owned()),
        };

        let resolved_dev = rule
            .device
            .as_deref()
            .and_then(|t| resolve_configured_device(configured_devices, t).ok());

        let mut view = AcsBridgeRule {
            id: rule.id.clone(),
            name: rule.name.clone(),
            scope: rule.scope.as_str().into(),
            target_ip: target_label,
            target_port: rule.target_port,
            path: path_val,
            path_mode: rule_path_mode,
            device: resolved_dev
                .map(|d| d.id.clone())
                .or_else(|| rule.device.clone()),
            optical_sn: resolved_dev.map(|d| d.optical_sn.clone()),
            interfaces: Vec::new(),
            acs_target: None,
            bridge_rules: Vec::new(),
            ip_rules: Vec::new(),
            state: if !config.enabled || !rule.enabled {
                "disabled".into()
            } else {
                "waiting".into()
            },
            reason: (!rule.enabled).then(|| "Rule is disabled".into()),
        };

        let mut planned = PlannedRule {
            config: rule.clone(),
            view: view.clone(),
            lan_bridge_rules: Vec::new(),
            lan_ip_rules: Vec::new(),
            mgmt_ip_rules: Vec::new(),
            matchers: Vec::new(),
        };

        if !config.enabled || !rule.enabled {
            planned_rules.push(planned);
            continue;
        }

        orig_ports.insert(rule.target_port);

        let onu_res = resolve_device_onus(
            rule.device.as_deref(),
            online_onus,
            interfaces,
            configured_devices,
        );
        let (matched_dev, onu_ifaces, onu_err) = match onu_res {
            Ok((dev, ifaces)) => (dev, ifaces, None),
            Err(err) => (resolved_dev.cloned(), Vec::new(), Some(err)),
        };
        if let Some(dev) = &matched_dev {
            view.device = Some(dev.id.clone());
            view.optical_sn = Some(dev.optical_sn.clone());
        }

        let mut reasons = Vec::new();
        let mut acs_targets = Vec::new();
        let mut rule_matchers = Vec::new();
        let mut rule_bridge = Vec::new();
        let mut rule_lan_ip = Vec::new();
        let mut rule_mgmt_ip = Vec::new();

        if rule.scope.includes_lan() {
            if let Some(err) = listener_error {
                reasons.push(format!("LAN ACS listener error: {err}"));
            } else if let (Some((lan_ip, port)), Some(bridge_mac)) =
                (lan_target_addr, lan.bridge_mac)
            {
                if !onu_ifaces.is_empty() {
                    let mut rule_err = None;
                    let mut iface_exprs = Vec::new();
                    for iface in &onu_ifaces {
                        match nft::render_lan_bridge_rule(
                            iface,
                            rule.target_ip,
                            rule.target_prefix,
                            rule.target_port,
                            bridge_mac,
                        ) {
                            Ok(expr) => iface_exprs.push((iface.clone(), expr)),
                            Err(e) => {
                                rule_err = Some(e);
                                break;
                            }
                        }
                    }
                    if let Some(e) = rule_err {
                        reasons.push(e);
                    } else {
                        for (iface, expr) in iface_exprs {
                            bridge_rules_set.insert(expr.clone());
                            view.bridge_rules.push(expr.clone());
                            rule_bridge.push(expr);
                            view.interfaces.push(iface);
                        }
                        let ip_expr = nft::render_lan_ip_rule(
                            rule.target_ip,
                            rule.target_prefix,
                            rule.target_port,
                            lan_ip,
                            port,
                        );
                        ip_rules_set.insert(ip_expr.clone());
                        view.ip_rules.push(ip_expr.clone());
                        rule_lan_ip.push(ip_expr);
                        acs_targets.push(format!("{lan_ip}:{port}"));

                        let m = conntrack::FlowMatcher {
                            orig_src: None,
                            orig_dst: rule.target_ip,
                            orig_dst_prefix: rule.target_prefix,
                            orig_port: rule.target_port,
                            reply_target: (lan_ip, port),
                        };
                        let key = matcher_key(&m);
                        desired_matchers.insert(key, m);
                        rule_matchers.push((key, m));
                    }
                } else if let Some(err) = &onu_err {
                    reasons.push(err.clone());
                }
            } else {
                reasons.push(
                    lan.reason
                        .clone()
                        .unwrap_or_else(|| "waiting for br-lan IPv4 address and MAC".into()),
                );
            }
        }

        if rule.scope.includes_management() {
            if let Some((mgmt_ip, mgmt_prefix, mgmt_port)) = mgmt_target_addr {
                let source_ip_res: Result<Option<Ipv4Addr>, String> = if rule.device.is_some() {
                    if let Some(err) = &onu_err {
                        Err(err.clone())
                    } else if let Some(dev) = &matched_dev {
                        resolve_management_source_ip(
                            dev,
                            mgmt_ip,
                            mgmt_prefix,
                            mgmt_leases,
                            source_links,
                            fdb_macs_for_serial,
                            acs_devices,
                            configured_devices,
                        )
                        .map(Some)
                    } else {
                        Err("configured FTTR ONU not found".into())
                    }
                } else {
                    Ok(None)
                };

                match source_ip_res {
                    Ok(source_ip) => {
                        let ip_expr = nft::render_management_ip_rule(
                            source_ip,
                            rule.target_ip,
                            rule.target_prefix,
                            rule.target_port,
                            mgmt_ip,
                            mgmt_port,
                        );
                        ip_rules_set.insert(ip_expr.clone());
                        view.ip_rules.push(ip_expr.clone());
                        rule_mgmt_ip.push(ip_expr);
                        view.interfaces.push(match source_ip {
                            Some(ip) => format!("br-tr069 ({ip})"),
                            None => "br-tr069".into(),
                        });
                        acs_targets.push(format!("{mgmt_ip}:{mgmt_port}"));

                        let m = conntrack::FlowMatcher {
                            orig_src: source_ip,
                            orig_dst: rule.target_ip,
                            orig_dst_prefix: rule.target_prefix,
                            orig_port: rule.target_port,
                            reply_target: (mgmt_ip, mgmt_port),
                        };
                        let key = matcher_key(&m);
                        desired_matchers.insert(key, m);
                        rule_matchers.push((key, m));
                    }
                    Err(err) => reasons.push(err),
                }
            } else {
                reasons.push("waiting for isolated TR069 management network".into());
            }
        }

        if !acs_targets.is_empty() {
            view.acs_target = Some(acs_targets.join(", "));
            view.state = "active".into();
            if !reasons.is_empty() {
                view.reason = Some(reasons.join("; "));
            }
        } else {
            view.state = if (listener_error.is_some() && rule.scope.includes_lan())
                || onu_err
                    .as_deref()
                    .is_some_and(|e| e.starts_with("unknown") || e.starts_with("ambiguous"))
            {
                "error".into()
            } else {
                "waiting".into()
            };
            if !reasons.is_empty() {
                view.reason = Some(reasons.join("; "));
            }
        }

        planned.view = view;
        planned.lan_bridge_rules = rule_bridge;
        planned.lan_ip_rules = rule_lan_ip;
        planned.mgmt_ip_rules = rule_mgmt_ip;
        planned.matchers = rule_matchers;
        planned_rules.push(planned);
    }

    Plan {
        lan_target_addr,
        mgmt_target_addr: mgmt_target_addr.map(|(ip, _, port)| (ip, port)),
        bridge_rules: bridge_rules_set.into_iter().collect(),
        ip_rules: ip_rules_set.into_iter().collect(),
        desired_matchers,
        orig_ports,
        rules: planned_rules,
        primary_path,
    }
}

struct FinalizedState {
    views: Vec<AcsBridgeRule>,
    mgmt_policy: PathPolicy,
    lan_policy: PathPolicy,
    path_mode: String,
    accepted_paths: Vec<String>,
    active_rules: u32,
}

fn finalize_from_applied(
    plan: &Plan,
    applied_bridge: &[String],
    applied_ip: &[String],
    tables_ok: Result<(bool, bool), String>,
    rule_error: Option<&str>,
) -> FinalizedState {
    let (bridge_ok, ip_ok) = tables_ok.unwrap_or((false, false));
    let applied_bridge_set: BTreeSet<&str> = applied_bridge.iter().map(String::as_str).collect();
    let applied_ip_set: BTreeSet<&str> = applied_ip.iter().map(String::as_str).collect();

    let mut views = Vec::with_capacity(plan.rules.len());
    let mut active_lan_configs = Vec::new();
    let mut active_mgmt_configs = Vec::new();

    for planned in &plan.rules {
        let mut view = planned.view.clone();
        if view.state == "active" {
            let lan_wanted =
                !planned.lan_bridge_rules.is_empty() || !planned.lan_ip_rules.is_empty();
            let lan_installed = lan_wanted
                && bridge_ok
                && ip_ok
                && planned
                    .lan_bridge_rules
                    .iter()
                    .all(|r| applied_bridge_set.contains(r.as_str()))
                && planned
                    .lan_ip_rules
                    .iter()
                    .all(|r| applied_ip_set.contains(r.as_str()));

            let mgmt_wanted = !planned.mgmt_ip_rules.is_empty();
            let mgmt_installed = mgmt_wanted
                && ip_ok
                && planned
                    .mgmt_ip_rules
                    .iter()
                    .all(|r| applied_ip_set.contains(r.as_str()));

            if lan_installed {
                active_lan_configs.push(&planned.config);
            }
            if mgmt_installed {
                active_mgmt_configs.push(&planned.config);
            }

            if !lan_installed && !mgmt_installed {
                view.state = "error".into();
                if let Some(err) = rule_error {
                    view.reason = Some(err.to_owned());
                } else if view.reason.is_none() {
                    view.reason =
                        Some("redirect nftables rules are not active in data plane".into());
                }
            }
        }
        views.push(view);
    }

    let (mgmt_policy, _, mgmt_paths) =
        build_listener_path_policy(&active_mgmt_configs, &plan.primary_path);
    let (lan_policy, _, lan_paths) =
        build_listener_path_policy(&active_lan_configs, &plan.primary_path);

    let mut active_configs = Vec::new();
    active_configs.extend(active_mgmt_configs);
    active_configs.extend(active_lan_configs);
    let (_, path_mode, _) = build_listener_path_policy(&active_configs, &plan.primary_path);

    let mut combined_paths = BTreeSet::new();
    for p in mgmt_paths.into_iter().chain(lan_paths) {
        combined_paths.insert(p);
    }
    let active_rules = views.iter().filter(|r| r.state == "active").count() as u32;

    FinalizedState {
        views,
        mgmt_policy,
        lan_policy,
        path_mode,
        accepted_paths: combined_paths.into_iter().collect(),
        active_rules,
    }
}

fn try_cleanup_data_plane<R, T, C>(
    c: &mut Coordinator,
    mut remove_nft: R,
    mut check_tables: T,
    mut cleanup_conntrack: C,
) -> Result<(), String>
where
    R: FnMut() -> Result<(), String>,
    T: FnMut() -> Result<(bool, bool), String>,
    C: FnMut(&[conntrack::FlowMatcher], &[conntrack::FallbackSweep<'_>]) -> Result<usize, String>,
{
    let initial_tables = check_tables();
    let had_tables = match initial_tables {
        Ok((b, i)) => b || i,
        Err(_) => true,
    };
    let had_tracked = !c.tracked_matchers.is_empty();
    let had_applied = !c.applied_bridge_rules.is_empty() || !c.applied_ip_rules.is_empty();

    if !had_tables && !had_tracked && !had_applied {
        c.cleanup_pending = false;
        c.cleanup_error = None;
        return Ok(());
    }

    let mut errors = Vec::new();
    if let Err(err) = remove_nft() {
        errors.push(err);
    }
    match check_tables() {
        Ok((false, false)) => {
            c.applied_bridge_rules.clear();
            c.applied_ip_rules.clear();
        }
        Ok((b, i)) => errors.push(format!(
            "nft cleanup incomplete (bridge_present={b}, ip_present={i})"
        )),
        Err(err) => errors.push(err),
    }

    let matchers: Vec<_> = c.tracked_matchers.values().copied().collect();
    let ports: Vec<u16> = if c.known_orig_ports.is_empty() {
        vec![DEFAULT_ACS_PORT]
    } else {
        c.known_orig_ports.iter().copied().collect()
    };
    let fallback: Vec<conntrack::FallbackSweep<'_>> = if matchers.is_empty() && had_tables {
        c.known_targets
            .iter()
            .map(|&(reply_ip, reply_port)| conntrack::FallbackSweep {
                reply_ip,
                reply_port,
                allowed_orig_ports: &ports,
            })
            .collect()
    } else {
        Vec::new()
    };

    if errors.is_empty() && (!matchers.is_empty() || !fallback.is_empty()) {
        match cleanup_conntrack(&matchers, &fallback) {
            Ok(_) => c.tracked_matchers.clear(),
            Err(err) => errors.push(err),
        }
    }

    if errors.is_empty() {
        c.cleanup_pending = false;
        c.cleanup_error = None;
        Ok(())
    } else {
        let msg = errors.join("; ");
        c.cleanup_pending = true;
        c.cleanup_error = Some(msg.clone());
        Err(msg)
    }
}

pub(crate) fn configure(config: &AcsBridgeConfig) {
    {
        let mut c = coordinator().lock().unwrap();
        c.config = config.clone();
        for rule in &config.rules {
            c.known_orig_ports.insert(rule.target_port);
        }
        c.status.enabled = config.enabled;
        c.status.management_gateway = config.management_gateway;
        if !config.enabled {
            c.cleanup_pending = true;
        }
    }

    if !config.enabled {
        crate::acs::set_redirect_paths(PathPolicy::default(), PathPolicy::default());
        let _ = crate::acs::configure_extra_listener(None);
        let mut c = coordinator().lock().unwrap();
        match try_cleanup_data_plane(
            &mut c,
            nft::remove,
            nft::tables_present,
            conntrack::cleanup_redirected_flows,
        ) {
            Ok(()) => {
                c.rule_error = None;
                c.listener_error = None;
                c.status = AcsBridgeStatus {
                    enabled: false,
                    management_gateway: config.management_gateway,
                    state: "disabled".into(),
                    lan_origin: origin_str(c.lan.origin).into(),
                    path_mode: "none".into(),
                    ..Default::default()
                };
            }
            Err(err) => {
                c.status.enabled = false;
                c.status.management_gateway = config.management_gateway;
                c.status.state = "error".into();
                c.status.error = Some(err);
                c.status.path_mode = "none".into();
                c.status.accepted_paths.clear();
            }
        }
    }
}

pub(crate) fn shutdown() {
    crate::acs::set_redirect_paths(PathPolicy::default(), PathPolicy::default());
    let _ = crate::acs::configure_extra_listener(None);
    let mut c = coordinator().lock().unwrap();
    c.cleanup_pending = true;
    match try_cleanup_data_plane(
        &mut c,
        nft::remove,
        nft::tables_present,
        conntrack::cleanup_redirected_flows,
    ) {
        Ok(()) => {
            c.status.state = "disabled".into();
            c.status.active_rules = 0;
            c.status.error = None;
        }
        Err(err) => {
            c.status.state = "error".into();
            c.status.error = Some(err);
        }
    }
}

pub(crate) fn tick(
    online_onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
    configured_devices: &[Device],
    management_network: Option<&Desired>,
    management_active: bool,
    acs_devices: &crate::cwmp::observations::DeviceObservations,
    source_links: Option<&BTreeMap<String, String>>,
) {
    let config = {
        let mut c = coordinator().lock().unwrap();
        if !c.config.enabled {
            if c.cleanup_pending {
                match try_cleanup_data_plane(
                    &mut c,
                    nft::remove,
                    nft::tables_present,
                    conntrack::cleanup_redirected_flows,
                ) {
                    Ok(()) => {
                        c.rule_error = None;
                        c.listener_error = None;
                        c.status.state = "disabled".into();
                        c.status.active_rules = 0;
                        c.status.error = None;
                        c.status.rules.clear();
                    }
                    Err(err) => {
                        c.status.state = "error".into();
                        c.status.error = Some(err);
                    }
                }
            }
            return;
        }
        c.config.clone()
    };

    let lan = inspect_lan_context();
    let acs_port = management_network
        .map(|m| m.port)
        .unwrap_or(DEFAULT_ACS_PORT);

    let need_lan_listener = config
        .rules
        .iter()
        .any(|r| r.enabled && r.scope.includes_lan());
    let lan_listen_target = lan.lan_ip.map(|ip| format!("{ip}:{acs_port}"));
    let listener_result = if need_lan_listener {
        if let Some(target) = &lan_listen_target {
            crate::acs::configure_extra_listener(Some(target)).map(|()| Some(target.clone()))
        } else {
            let _ = crate::acs::configure_extra_listener(None);
            Ok(None)
        }
    } else {
        let _ = crate::acs::configure_extra_listener(None);
        Ok(None)
    };
    let (lan_listen, listener_error) = match listener_result {
        Ok(addr) => (addr, None),
        Err(err) => (None, Some(err)),
    };

    let mgmt_leases = management_network
        .filter(|m| management_active && m.enabled)
        .and_then(|m| {
            m.address
                .parse::<Ipv4Addr>()
                .ok()
                .map(|ip| read_subnet_leases(ip, m.prefix))
        })
        .unwrap_or_default();

    let eval_time = now_secs();
    let fdb_macs_for_serial = match crate::mqtt_identity::shared().lock() {
        Ok(reg) => configured_devices
            .iter()
            .map(|d| {
                (
                    d.optical_sn.clone(),
                    reg.fdb_candidates_for_serial(&d.optical_sn, eval_time),
                )
            })
            .collect(),
        Err(_) => BTreeMap::new(),
    };

    let plan = build_plan(
        &config,
        &lan,
        online_onus,
        interfaces,
        configured_devices,
        management_network,
        management_active,
        &mgmt_leases,
        source_links,
        &fdb_macs_for_serial,
        acs_devices,
        listener_error.as_deref(),
    );

    let mut c = coordinator().lock().unwrap();
    c.lan = lan.clone();
    c.listener_error = listener_error.clone();
    for port in &plan.orig_ports {
        c.known_orig_ports.insert(*port);
    }
    if let Some(t) = plan.lan_target_addr {
        if !c.known_targets.contains(&t) {
            c.known_targets.push(t);
        }
    }
    if let Some(t) = plan.mgmt_target_addr {
        if !c.known_targets.contains(&t) {
            c.known_targets.push(t);
        }
    }

    let initial_tables = nft::tables_present();
    let want_empty = plan.bridge_rules.is_empty() && plan.ip_rules.is_empty();
    let mut rule_error = None;
    let mut cleanup_error = None;

    if want_empty {
        match try_cleanup_data_plane(
            &mut c,
            nft::remove,
            nft::tables_present,
            conntrack::cleanup_redirected_flows,
        ) {
            Ok(()) => {}
            Err(err) => cleanup_error = Some(err),
        }
    } else {
        let needs_sync = match initial_tables {
            Ok((b, i)) => {
                (!plan.bridge_rules.is_empty() && !b)
                    || (!plan.ip_rules.is_empty() && !i)
                    || (plan.bridge_rules.is_empty() && b)
                    || (plan.ip_rules.is_empty() && i)
                    || c.applied_bridge_rules != plan.bridge_rules
                    || c.applied_ip_rules != plan.ip_rules
            }
            Err(_) => true,
        };

        if needs_sync {
            match nft::apply(&plan.bridge_rules, &plan.ip_rules) {
                Ok(()) => {
                    c.applied_bridge_rules = plan.bridge_rules.clone();
                    c.applied_ip_rules = plan.ip_rules.clone();
                    for (k, m) in &plan.desired_matchers {
                        c.tracked_matchers.insert(*k, *m);
                    }
                    c.cleanup_pending = false;
                }
                Err(err) => {
                    rule_error = Some(err);
                }
            }
        }

        let removed_matchers: Vec<_> = c
            .tracked_matchers
            .iter()
            .filter(|(k, _)| !plan.desired_matchers.contains_key(*k))
            .map(|(_, m)| *m)
            .collect();
        if !removed_matchers.is_empty() && rule_error.is_none() {
            match conntrack::cleanup_redirected_flows(&removed_matchers, &[]) {
                Ok(_) => {
                    c.tracked_matchers
                        .retain(|k, _| plan.desired_matchers.contains_key(k));
                }
                Err(err) => {
                    cleanup_error = Some(err);
                }
            }
        }
    }

    c.rule_error = rule_error.clone();
    c.cleanup_error = cleanup_error.clone();

    let final_tables = nft::tables_present();
    let finalized = finalize_from_applied(
        &plan,
        &c.applied_bridge_rules,
        &c.applied_ip_rules,
        final_tables,
        rule_error.as_deref(),
    );

    crate::acs::set_redirect_paths(finalized.mgmt_policy, finalized.lan_policy);

    let combined_error = listener_error.or(rule_error).or(cleanup_error);
    let state = if combined_error.is_some() {
        "error"
    } else if finalized.active_rules > 0 {
        "active"
    } else {
        "waiting"
    };
    let reason = if state == "waiting" {
        if config.rules.is_empty() {
            Some("No ACS redirect rules configured".into())
        } else if config.rules.iter().all(|r| !r.enabled) {
            Some("All ACS redirect rules are disabled".into())
        } else {
            finalized.views.iter().find_map(|r| r.reason.clone())
        }
    } else {
        None
    };

    c.status = AcsBridgeStatus {
        enabled: true,
        management_gateway: config.management_gateway,
        state: state.into(),
        lan_origin: origin_str(lan.origin).into(),
        lan_listen,
        management_listen: plan
            .mgmt_target_addr
            .map(|(ip, port)| format!("{ip}:{port}")),
        path_mode: finalized.path_mode,
        accepted_paths: finalized.accepted_paths,
        active_rules: finalized.active_rules,
        rules: finalized.views,
        reason,
        error: combined_error,
    };
}

pub(crate) fn global_view() -> Value {
    let c = coordinator().lock().unwrap();
    serde_json::to_value(&c.status).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::redirect_net::lan::LanOrigin;
    use crate::provisioning::model::AcsRedirectScope;
    use serde_json::json;

    fn test_device(id: &str, sn: &str) -> Device {
        Device {
            id: id.into(),
            optical_sn: sn.into(),
            cwmp_id: String::new(),
            omci_adapter: "hl202".into(),
            bootstrap_adapter: "omci-acs".into(),
            provisioning_adapter: "generic-cwmp".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            items: Vec::new(),
            wifi_cleanup: String::new(),
            wifi_preserve: BTreeSet::new(),
        }
    }

    fn test_mgmt() -> Desired {
        serde_json::from_value(json!({
            "management": {
                "vlan": 46,
                "vlans": [46],
                "priority": 0,
                "acs_url": "http://192.0.2.1:7547/cwmp"
            },
            "enabled": true,
            "automatic": true,
            "address": "192.0.2.1",
            "prefix": 24,
            "dhcp": true,
            "start": 100,
            "limit": 100,
            "lease": "12h",
            "acs_enabled": true,
            "port": 7547,
            "path": "/cwmp",
            "username": "",
            "password": "",
            "management_gateway": true
        }))
        .unwrap()
    }

    #[test]
    fn management_device_rule_isolates_source_ip_and_waits_when_unattributable() {
        let config = AcsBridgeConfig {
            enabled: true,
            management_gateway: true,
            rules: vec![AcsRedirectRuleConfig {
                id: "dev_a".into(),
                name: Some("ONU A Mgmt".into()),
                enabled: true,
                scope: AcsRedirectScope::Management,
                target_ip: Ipv4Addr::new(10, 20, 30, 40),
                target_prefix: 32,
                target_port: 9090,
                path: AcsPathMatch::Exact("/tr069".into()),
                device: Some("H3CT00000001".into()),
            }],
        };
        let online_onus = BTreeMap::from([(0, "H3CT00000001".into()), (1, "H3CT00000002".into())]);
        let interfaces = BTreeMap::from([(0, "fttr0".into()), (1, "fttr1".into())]);
        let devices = vec![
            test_device("onu1", "H3CT00000001"),
            test_device("onu2", "H3CT00000002"),
        ];
        let mgmt = test_mgmt();
        let mgmt_leases = BTreeMap::from([
            ([0x02, 0, 0, 0, 0, 0x11], Ipv4Addr::new(192, 0, 2, 101)),
            ([0x02, 0, 0, 0, 0, 0x22], Ipv4Addr::new(192, 0, 2, 102)),
        ]);

        // Without link attribution or ACS peer, management device rule MUST wait (never hijack onu2).
        let unattributed = build_plan(
            &config,
            &LanContext::default(),
            &online_onus,
            &interfaces,
            &devices,
            Some(&mgmt),
            true,
            &mgmt_leases,
            None,
            &BTreeMap::new(),
            &Default::default(),
            None,
        );
        assert_eq!(unattributed.rules[0].view.state, "waiting");
        assert!(unattributed.ip_rules.is_empty());
        assert!(unattributed.desired_matchers.is_empty());

        // With source_links attributing MAC 02:...:11 to H3CT00000001, rule scopes strictly to ip saddr 192.0.2.101.
        let source_links = BTreeMap::from([
            ("020000000011".into(), "H3CT00000001".into()),
            ("020000000022".into(), "H3CT00000002".into()),
        ]);
        let attributed = build_plan(
            &config,
            &LanContext::default(),
            &online_onus,
            &interfaces,
            &devices,
            Some(&mgmt),
            true,
            &mgmt_leases,
            Some(&source_links),
            &BTreeMap::new(),
            &Default::default(),
            None,
        );
        assert_eq!(attributed.rules[0].view.state, "active");
        assert_eq!(attributed.rules[0].view.device.as_deref(), Some("onu1"));
        assert!(attributed.ip_rules[0].contains("ip saddr 192.0.2.101"));
        assert!(!attributed.ip_rules[0].contains("192.0.2.102"));
        let matcher = attributed.desired_matchers.values().next().unwrap();
        assert_eq!(matcher.orig_src, Some(Ipv4Addr::new(192, 0, 2, 101)));
        assert_eq!(matcher.reply_target, (Ipv4Addr::new(192, 0, 2, 1), 7547));
    }

    #[test]
    fn failed_nft_apply_does_not_mark_rule_active_and_splits_listener_policies() {
        let config = AcsBridgeConfig {
            enabled: true,
            management_gateway: true,
            rules: vec![
                AcsRedirectRuleConfig {
                    id: "lan_wild".into(),
                    name: None,
                    enabled: true,
                    scope: AcsRedirectScope::Lan,
                    target_ip: Ipv4Addr::new(10, 20, 30, 0),
                    target_prefix: 24,
                    target_port: 7547,
                    path: AcsPathMatch::Wildcard,
                    device: Some("onu1".into()),
                },
                AcsRedirectRuleConfig {
                    id: "mgmt_exact".into(),
                    name: None,
                    enabled: true,
                    scope: AcsRedirectScope::Management,
                    target_ip: Ipv4Addr::new(198, 51, 100, 10),
                    target_prefix: 32,
                    target_port: 8080,
                    path: AcsPathMatch::Exact("/tr069".into()),
                    device: None,
                },
            ],
        };
        let lan = LanContext {
            origin: LanOrigin::Upstream,
            lan_ip: Some(Ipv4Addr::new(192, 0, 2, 44)),
            lan_mask: 24,
            bridge_mac: Some([0x02, 0x00, 0x00, 0x00, 0x00, 0xaa]),
            ..Default::default()
        };
        let online_onus = BTreeMap::from([(0, "H3CT00000001".into())]);
        let interfaces = BTreeMap::from([(0, "fttr0".into())]);
        let devices = vec![test_device("onu1", "H3CT00000001")];
        let mgmt = test_mgmt();

        let plan = build_plan(
            &config,
            &lan,
            &online_onus,
            &interfaces,
            &devices,
            Some(&mgmt),
            true,
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &Default::default(),
            None,
        );

        // When applied succeeds, LAN gets wildcard without widening Management (which stays exact /tr069).
        let ok = finalize_from_applied(
            &plan,
            &plan.bridge_rules,
            &plan.ip_rules,
            Ok((true, true)),
            None,
        );
        assert_eq!(ok.active_rules, 2);
        assert!(!ok.mgmt_policy.allow_any);
        assert_eq!(ok.mgmt_policy.extra_paths, vec!["/tr069"]);
        assert!(ok.lan_policy.allow_any);

        // When nft apply failed (no rules applied), active_rules is 0 and views report "error", NOT "active".
        let failed = finalize_from_applied(&plan, &[], &[], Ok((false, false)), Some("nft failed"));
        assert_eq!(failed.active_rules, 0);
        assert!(failed.views.iter().all(|v| v.state == "error"));
        assert!(!failed.lan_policy.allow_any);
        assert!(failed.mgmt_policy.extra_paths.is_empty());
    }

    #[test]
    fn cleanup_retains_pending_state_on_failure_and_clears_when_verified() {
        let m = conntrack::FlowMatcher {
            orig_src: Some(Ipv4Addr::new(192, 0, 2, 101)),
            orig_dst: Ipv4Addr::new(10, 0, 0, 1),
            orig_dst_prefix: 32,
            orig_port: 9090,
            reply_target: (Ipv4Addr::new(192, 0, 2, 1), 7547),
        };
        let mut c = Coordinator {
            applied_bridge_rules: vec!["dummy".into()],
            applied_ip_rules: vec!["dummy".into()],
            tracked_matchers: BTreeMap::from([(matcher_key(&m), m)]),
            known_targets: vec![(Ipv4Addr::new(192, 0, 2, 1), 7547)],
            cleanup_pending: true,
            ..Default::default()
        };

        // Failure on nft remove keeps cleanup_pending and tracked_matchers for retry.
        let err = try_cleanup_data_plane(
            &mut c,
            || Err("nft busy".into()),
            || Ok((true, true)),
            |_, _| Ok(0),
        )
        .unwrap_err();
        assert!(err.contains("nft busy"));
        assert!(c.cleanup_pending);
        assert_eq!(c.tracked_matchers.len(), 1);
        assert_eq!(c.applied_bridge_rules.len(), 1);

        // Retry succeeds once both tables are absent and conntrack cleanup succeeds.
        assert!(try_cleanup_data_plane(
            &mut c,
            || Ok(()),
            || Ok((false, false)),
            |matchers, _| {
                assert_eq!(matchers.len(), 1);
                Ok(1)
            },
        )
        .is_ok());
        assert!(!c.cleanup_pending);
        assert!(c.tracked_matchers.is_empty());
        assert!(c.applied_bridge_rules.is_empty());
        assert!(c.applied_ip_rules.is_empty());
    }

    #[test]
    fn rejects_ambiguous_multi_interface_onu_matches() {
        let online_onus = BTreeMap::from([(0, "H3CT00000001".into()), (1, "H3CT00000001".into())]);
        let interfaces = BTreeMap::from([(0, "fttr0".into()), (1, "fttr1".into())]);
        let devices = vec![test_device("onu1", "H3CT00000001")];
        let err = resolve_device_onus(Some("onu1"), &online_onus, &interfaces, &devices)
            .err()
            .unwrap();
        assert!(err.contains("ambiguous online interfaces"));
    }
}
