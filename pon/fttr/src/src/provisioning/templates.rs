//! Public group/template model. Expanded sections are private compiler output.
use super::config::{self, Config, Section};
use std::collections::{BTreeMap, BTreeSet};

fn emit(out: &mut Vec<(String, Section)>, kind: &str, id: &str, s: &Section) {
    let mut section = s.clone();
    section.kind = kind.into();
    out.push((id.into(), section));
}

// Stable, quoted encoding for business revisions only; never reparsed as input.
fn canonical_section(out: &mut String, id: &str, s: &Section) {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    out.push_str(&format!("config {} {}\n", quote(&s.kind), quote(id)));
    for (key, values) in &s.options {
        for value in values {
            out.push_str(&format!(
                " {} {} {}\n",
                if values.len() > 1 { "list" } else { "option" },
                quote(key),
                quote(value)
            ));
        }
    }
}

fn section(kind: &str, values: &[(&str, &str)]) -> Section {
    Section {
        kind: kind.into(),
        options: values
            .iter()
            .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
            .collect(),
    }
}
fn list<'a>(s: &'a Section, key: &str) -> impl Iterator<Item = &'a str> {
    s.options.get(key).into_iter().flatten().map(String::as_str)
}
fn get<'a>(all: &'a [(String, Section)], id: &str, kind: &str) -> Result<&'a Section, String> {
    all.iter()
        .find(|(n, s)| n == id && s.kind == kind)
        .map(|(_, s)| s)
        .ok_or_else(|| format!("unknown {kind}: {id}"))
}

fn parse_bool_flag(raw: &str, default: bool, label: &str) -> Result<bool, String> {
    match raw {
        "" => Ok(default),
        "0" | "false" => Ok(false),
        "1" | "true" => Ok(true),
        _ => Err(format!("{label} must be 0 or 1")),
    }
}

fn parse_target_cidr(raw: &str) -> Result<(std::net::Ipv4Addr, u8), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("acs_redirect target_ip is required".into());
    }
    let (ip_str, prefix) = match trimmed.split_once('/') {
        Some((ip_part, prefix_part)) => {
            if prefix_part.is_empty() || !prefix_part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid acs_redirect CIDR prefix: {trimmed}"));
            }
            let prefix = prefix_part
                .parse::<u8>()
                .ok()
                .filter(|p| (1..=32).contains(p))
                .ok_or_else(|| format!("acs_redirect CIDR prefix must be 1..32: {trimmed}"))?;
            (ip_part, prefix)
        }
        None => (trimmed, 32),
    };
    let ip: std::net::Ipv4Addr = ip_str
        .parse()
        .map_err(|_| format!("invalid acs_redirect target_ip: {trimmed}"))?;
    if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return Err(format!("invalid acs_redirect target_ip: {trimmed}"));
    }
    let mask = if prefix == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    Ok((std::net::Ipv4Addr::from(u32::from(ip) & mask), prefix))
}

fn parse_acs_bridge(all: &[(String, Section)]) -> Result<super::model::AcsBridgeConfig, String> {
    use super::model::{AcsBridgeConfig, AcsPathMatch, AcsRedirectRuleConfig, AcsRedirectScope};
    let switches: Vec<_> = all.iter().filter(|(_, s)| s.kind == "acs_bridge").collect();
    if switches.len() > 1 || switches.first().is_some_and(|(id, _)| id != "acs_bridge") {
        return Err("acs_bridge requires one named acs_bridge section".into());
    }
    let (enabled, management_gateway) = if let Some((_, s)) = switches.first() {
        (
            parse_bool_flag(s.get("enabled"), false, "acs_bridge enabled")?,
            parse_bool_flag(
                s.get("management_gateway"),
                false,
                "acs_bridge management_gateway",
            )?,
        )
    } else {
        (false, false)
    };

    let mut rules = Vec::new();
    for (id, s) in all.iter().filter(|(_, s)| s.kind == "acs_redirect") {
        let rule_enabled = parse_bool_flag(
            s.get("enabled"),
            true,
            &format!("acs_redirect {id} enabled"),
        )?;
        let scope = match s.get("scope") {
            "" | "lan" => AcsRedirectScope::Lan,
            "management" => AcsRedirectScope::Management,
            "both" => AcsRedirectScope::Both,
            other => {
                return Err(format!(
                    "acs_redirect {id} unsupported scope {other}; expected lan, management or both"
                ))
            }
        };
        let (target_ip, target_prefix) = parse_target_cidr(s.get("target_ip"))?;
        let port_raw = s.get("target_port");
        let target_port = if port_raw.is_empty() {
            7547
        } else {
            if !port_raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("acs_redirect {id} invalid target_port: {port_raw}"));
            }
            port_raw
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| format!("acs_redirect {id} target_port must be 1..65535"))?
        };
        let path_raw = s.get("path").trim();
        let path = if path_raw.is_empty() {
            return Err(format!(
                "acs_redirect {id} path is required: enter an exact path starting with '/' or '*' for wildcard"
            ));
        } else if path_raw == "*" {
            AcsPathMatch::Wildcard
        } else {
            if !path_raw.starts_with('/')
                || path_raw
                    .bytes()
                    .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
                || path_raw.contains(['?', '#'])
                || path_raw.contains("..")
                || path_raw.contains('\\')
            {
                return Err(format!("acs_redirect {id} invalid path: {path_raw}"));
            }
            AcsPathMatch::Exact(path_raw.to_owned())
        };
        let name = (!s.get("name").is_empty()).then(|| s.get("name").to_owned());
        let device = (!s.get("device").is_empty()).then(|| s.get("device").to_owned());
        rules.push(AcsRedirectRuleConfig {
            id: id.clone(),
            name,
            enabled: rule_enabled,
            scope,
            target_ip,
            target_prefix,
            target_port,
            path,
            device,
        });
    }
    Ok(AcsBridgeConfig {
        enabled,
        management_gateway,
        rules,
    })
}
pub fn compile(text: &str) -> Result<Config, String> {
    compile_view(&crate::config::View::parse(text)?)
}
pub(super) fn compile_view(view: &crate::config::View) -> Result<Config, String> {
    // Optical state belongs to the controller, not the expanded provisioning plan.
    // Share its validation so config check and optical startup accept the same input.
    crate::optical::settings_from_view(view).map_err(|error| error.to_string())?;
    crate::optical::calibration_from_view(view).map_err(|error| error.to_string())?;
    let all = config::sections_from_view(view)?;
    let fiberhome: Vec<_> = all.iter().filter(|(_, s)| s.kind == "fiberhome").collect();
    if fiberhome.len() > 1 || fiberhome.first().is_some_and(|(id, _)| id != "fiberhome") {
        return Err("fiberhome requires one named fiberhome section".into());
    }
    let onu_count = fiberhome
        .first()
        .map(|(_, s)| {
            let value = s.get("onu_count");
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("fiberhome onu_count must be 1..128".to_string());
            }
            value
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=128).contains(n))
                .ok_or_else(|| "fiberhome onu_count must be 1..128".to_string())
        })
        .transpose()?;
    if let Some((_, s)) = fiberhome.first() {
        if !matches!(s.get("rx_snapshot"), "" | "0" | "1" | "false" | "true") {
            return Err("fiberhome rx_snapshot must be 0 or 1".into());
        }
    }
    if all.iter().filter(|(_, s)| s.kind == "management").count() > 1 {
        return Err("only one management network allowed".into());
    }
    let mqtt_bridge: Vec<_> = all
        .iter()
        .filter(|(_, s)| s.kind == "mqtt_bridge")
        .collect();
    if mqtt_bridge.len() > 1
        || mqtt_bridge
            .first()
            .is_some_and(|(id, _)| id != "mqtt_bridge")
    {
        return Err("mqtt_bridge requires one named mqtt_bridge section".into());
    }
    let mqtt_bridge_enabled = match mqtt_bridge
        .first()
        .map(|(_, s)| s.get("enabled"))
        .unwrap_or("")
    {
        "" | "0" | "false" => false,
        "1" | "true" => true,
        _ => return Err("mqtt_bridge enabled must be 0 or 1".into()),
    };
    let acs_bridge = parse_acs_bridge(&all)?;
    // Validate every template, including ones not assigned to a group yet.
    for (id, s) in &all {
        if !matches!(
            s.kind.as_str(),
            "optical"
                | "calibration"
                | "acs"
                | "provisioning"
                | "management"
                | "device"
                | "group"
                | "template"
                | "network"
                | "wifi"
                | "offload"
                | "mqtt"
                | "mqtt_bridge"
                | "acs_bridge"
                | "acs_redirect"
                | "omci_auth"
                | "fiberhome"
        ) {
            return Err(format!("unsupported section: {}", s.kind));
        }
        let allowed: &[&str] = match s.kind.as_str() {
            "fiberhome" => &["onu_count", "rx_snapshot"],
            "mqtt_bridge" => &["enabled"],
            "acs_bridge" => &["enabled", "management_gateway"],
            "acs_redirect" => &[
                "enabled",
                "scope",
                "target_ip",
                "target_port",
                "path",
                "device",
                "name",
            ],
            "omci_auth" => &["auth_mode", "auth_loid", "auth_password", "schema_version"],
            "optical" => &["enabled", "downstream_tx_enabled", "fec_enabled"],
            "calibration" => &["source"],
            "device" => &[
                "auth_mode",
                "auth_loid",
                "auth_password",
                "name",
                "optical_sn",
                "onu_id",
                "omci_adapter",
                "bootstrap_adapter",
                "provisioning_adapter",
                "group",
                "wifi_cleanup",
                "connection_request",
                "connection_request_defaults",
                "connection_request_port",
                "connection_request_username",
                "connection_request_password",
            ],
            "group" => &["name", "template"],
            "template" => &["name"],
            "network" => &[
                "name",
                "template",
                "service_list",
                "vlan",
                "priority",
                "ssid",
                "port",
            ],
            "wifi" => &["template", "ssid", "radio", "security", "key"],
            "management" => &[
                "enabled",
                "mode",
                "vlan",
                "priority",
                "address",
                "dhcp_enabled",
                "dhcp_start",
                "dhcp_limit",
                "dhcp_leasetime",
            ],
            "provisioning" => &[
                "enabled",
                "allow_unsupported_as_bridge",
                "mode",
                "trigger",
                "interval_minutes",
                "freshness",
            ],
            "acs" => &[
                "enabled",
                "port",
                "path",
                "state_file",
                "username",
                "password",
            ],
            "offload" => &[
                "enabled",
                "bridge",
                "wan_upstream",
                "wan_downstream",
                "onu_bridge",
            ],
            "mqtt" => &["enabled", "hl202_enabled", "ty_fs803g_enabled"],
            _ => &[],
        };
        if let Some(key) = s.options.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!("{id}: unsupported option {key}"));
        }
        if s.kind == "group" {
            get(&all, s.get("template"), "template")?;
        }
        if matches!(s.kind.as_str(), "network" | "wifi") {
            get(&all, s.get("template"), "template")?;
        }
        if s.kind == "network" {
            let configured = s
                .options
                .get("service_list")
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let labels = config::normalize_custom_vlan_service_list(configured)?;
            if labels.is_empty() {
                return Err(format!("{id}: select at least one WAN Service List label"));
            }
            let vlan = s.number("vlan", 4094)?;
            if labels.iter().any(|label| label == "INTERNET") && vlan == 0 {
                return Err(format!(
                    "{id}: untagged INTERNET WAN is not managed by ACS automation"
                ));
            }
            if !labels.is_empty() && vlan == 0 {
                return Err(format!(
                    "{id}: custom VLAN ServiceList requires a non-zero VLAN ID"
                ));
            }
            if !s.get("priority").is_empty() {
                s.number("priority", 7)?;
            }
            for wifi in list(s, "ssid") {
                if get(&all, wifi, "wifi")?.get("template") != s.get("template") {
                    return Err(format!("{id}: SSID belongs to another template"));
                }
            }
            for port in list(s, "port") {
                if !matches!(port, "lan1" | "lan2" | "lan3" | "lan4") {
                    return Err(format!("{id}: invalid LAN port"));
                }
            }
        }
        if s.kind == "wifi" {
            if s.get("ssid").is_empty() || s.get("ssid").len() > 32 {
                return Err(format!("{id}: SSID must be 1..32 bytes"));
            }
            if !matches!(s.get("radio"), "2g" | "5g") {
                return Err(format!("{id}: invalid radio"));
            }
            match s.get("security") {
                "" | "none" => {}
                "wpa2-psk" | "wpa3-sae" | "wpa2-psk/wpa3-sae" | "wpa-psk/wpa2-psk" => {
                    if !(8..=63).contains(&s.get("key").len()) {
                        return Err(format!("{id}: invalid Wi-Fi security"));
                    }
                }
                _ => return Err(format!("{id}: invalid Wi-Fi security")),
            }
        }
        if s.kind == "device"
            && !matches!(
                s.get("wifi_cleanup"),
                "" | "preserve" | "disable" | "delete"
            )
        {
            return Err(format!(
                "{id}: wifi_cleanup must be preserve, disable or delete"
            ));
        }
    }
    for (template, _) in all.iter().filter(|(_, s)| s.kind == "template") {
        let mut bound = BTreeSet::new();
        for (_, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "network" && s.get("template") == template)
        {
            for endpoint in list(s, "ssid")
                .map(|s| format!("ssid:{s}"))
                .chain(list(s, "port").map(|s| format!("port:{s}")))
            {
                if !bound.insert(endpoint) {
                    return Err(format!("{template}: endpoint bound to multiple networks"));
                }
            }
        }
    }
    let mut expanded = Vec::new();
    let mut order = BTreeMap::<String, Vec<String>>::new();
    for (id, s) in all
        .iter()
        .filter(|(_, s)| matches!(s.kind.as_str(), "acs" | "provisioning"))
    {
        emit(&mut expanded, &s.kind, id, s);
    }
    for (device, d) in all.iter().filter(|(_, s)| s.kind == "device") {
        let group = get(&all, d.get("group"), "group")?;
        let template_id = group.get("template");
        get(&all, template_id, "template")?;
        let mut dev = d.clone();
        if dev.get("wifi_cleanup").is_empty() {
            dev.options
                .insert("wifi_cleanup".into(), vec!["disable".into()]);
        }
        dev.options.insert(
            "cwmp_id".into(),
            vec![format!(
                "pending:{}",
                d.get("optical_sn").to_ascii_uppercase()
            )],
        );
        emit(&mut expanded, "device", device, &dev);
        let mut wifi_instances = BTreeMap::new();
        let mut slots = [0u32; 2];
        let mut ids = Vec::new();
        for (id, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "wifi" && s.get("template") == template_id)
        {
            let band = if s.get("radio") == "2g" { 0 } else { 1 };
            slots[band] += 1;
            if slots[band] > 4 {
                return Err(format!("{template_id}: too many SSIDs for radio"));
            }
            wifi_instances.insert(id.clone(), slots[band] + if band == 1 { 4 } else { 0 });
            let profile = format!("{device}__profile__{id}");
            let mut p = s.clone();
            p.options
                .entry("enabled".into())
                .or_insert(vec!["1".into()]);
            p.options
                .entry("broadcast".into())
                .or_insert(vec!["1".into()]);
            emit(&mut expanded, "wifi_profile", &profile, &p);
            let name = format!("wifi__{id}");
            ids.push(name.clone());
            emit(
                &mut expanded,
                "ssid",
                &format!("{device}__{name}"),
                &section(
                    "ssid",
                    &[
                        ("device", device),
                        ("profile", &profile),
                        ("radio", s.get("radio")),
                        ("slot", &slots[band].to_string()),
                    ],
                ),
            );
        }
        let mut network_ids = Vec::new();
        let mut bindings = Vec::new();
        for (id, s) in all
            .iter()
            .filter(|(_, s)| s.kind == "network" && s.get("template") == template_id)
        {
            let service = format!("{device}__service__{id}");
            let configured = s.options.get("service_list").cloned().unwrap_or_default();
            let labels = config::normalize_custom_vlan_service_list(&configured)?;
            // Business classification is independent of the device's OMCI family.
            let mut service_config = s.clone();
            if service_config.get("type").is_empty() {
                service_config.options.insert(
                    "type".into(),
                    vec![if labels.iter().any(|label| label == "INTERNET") {
                        "internet".into()
                    } else {
                        "iptv".into()
                    }],
                );
            }
            emit(&mut expanded, "service", &service, &service_config);
            let name = format!("{device}__network__{id}");
            network_ids.push(name.clone());
            let label = labels.join(",");
            emit(
                &mut expanded,
                "device_service",
                &name,
                &section(
                    "device_service",
                    &[
                        ("device", device),
                        ("service", &service),
                        ("service_list", &label),
                        (
                            "enabled",
                            if s.get("enabled").is_empty() {
                                "1"
                            } else {
                                s.get("enabled")
                            },
                        ),
                    ],
                ),
            );
            let interfaces: Vec<String> = list(s, "ssid")
                .map(|id| {
                    format!(
                        "ssid:{}:{}",
                        if wifi_instances[id] <= 4 { 2 } else { 5 },
                        (wifi_instances[id] - 1) % 4 + 1
                    )
                })
                .chain(list(s, "port").map(|p| format!("lan:{}", &p[3..])))
                .collect();
            if !interfaces.is_empty() {
                let binding = format!("{device}__binding__{id}");
                bindings.push(binding.clone());
                let mut b = section(
                    "port_binding",
                    &[("device", device), ("device_service", &name)],
                );
                b.options.insert("interface".into(), interfaces);
                emit(&mut expanded, "port_binding", &binding, &b);
            }
        }
        network_ids.extend(ids.into_iter().map(|id| format!("{device}__{id}")));
        network_ids.extend(bindings);
        order.insert(device.clone(), network_ids);
    }
    let mut cfg = config::compile_sections(expanded)?;
    cfg.fiberhome_onu_count = onu_count;
    cfg.mqtt_bridge_enabled = mqtt_bridge_enabled;
    for rule in &acs_bridge.rules {
        if let Some(target) = &rule.device {
            super::model::resolve_configured_device(&cfg.devices, target)
                .map_err(|err| format!("acs_redirect {}: {err}", rule.id))?;
        }
    }
    cfg.acs_bridge = acs_bridge;
    if let Some(count) = onu_count {
        if cfg.onu_pins.values().any(|id| *id >= count) {
            return Err(format!(
                "configured ONU-ID exceeds fiberhome onu_count {count}"
            ));
        }
    }
    cfg.omci_auth = config::compile_auth(&all.iter().cloned().collect())?;
    if let Some((_, s)) = all.iter().find(|(_, s)| s.kind == "management") {
        let acs = all.iter().find(|(_, s)| s.kind == "acs").map(|(_, s)| s);
        let mut desired = super::management_network::Desired::parse(s, acs)?;
        desired.management_gateway = cfg!(feature = "acs-bridge")
            && cfg.acs_bridge.enabled
            && cfg.acs_bridge.management_gateway;
        desired.devices = cfg
            .devices
            .iter()
            .filter(|d| d.bootstrap_adapter != "none")
            .map(|d| (d.id.clone(), d.optical_sn.clone()))
            .collect();
        if all.iter().any(|(_, n)| {
            n.kind == "network"
                && n.get("vlan")
                    .parse::<u32>()
                    .ok()
                    .is_some_and(|v| desired.management.vlans.contains(&v))
        }) {
            return Err("management VLAN conflicts with a business VLAN".into());
        }
        cfg.management = Some(desired.management.clone());
        cfg.management_network = Some(desired);
    }
    cfg.groups = all
        .iter()
        .filter(|(_, s)| s.kind == "group")
        .map(|(id, s)| (id.clone(), s.get("template").into()))
        .collect();
    cfg.labels = all
        .iter()
        .map(|(id, s)| {
            (
                id.clone(),
                if !s.get("name").is_empty() {
                    s.get("name").into()
                } else if s.kind == "wifi" {
                    s.get("ssid").into()
                } else if s.kind == "device" {
                    s.get("optical_sn").into()
                } else {
                    id.clone()
                },
            )
        })
        .collect();
    cfg.assignments = all
        .iter()
        .filter(|(_, s)| s.kind == "device")
        .map(|(id, s)| {
            (
                id.clone(),
                (
                    s.get("group").into(),
                    get(&all, s.get("group"), "group")
                        .unwrap()
                        .get("template")
                        .into(),
                ),
            )
        })
        .collect();
    cfg.devices
        .sort_by_key(|d| all.iter().position(|(id, _)| id == &d.id).unwrap());
    for d in &mut cfg.devices {
        d.items
            .sort_by_key(|i| order[&d.id].iter().position(|id| id == &i.id).unwrap());
    }
    // Listener changes activate host configuration without scheduling business sync.
    let mut canonical = String::new();
    let mut business_canonical = String::new();
    for (id, s) in &all {
        if !matches!(s.kind.as_str(), "optical" | "calibration" | "fiberhome") {
            let mut business = s.clone();
            if business.kind == "device" {
                business.options.remove("onu_id");
            }
            canonical_section(&mut canonical, id, &business);
            if !matches!(
                business.kind.as_str(),
                "management" | "acs" | "omci_auth" | "mqtt_bridge" | "acs_bridge" | "acs_redirect"
            ) {
                if business.kind == "device" {
                    business.options.retain(|key, _| {
                        !matches!(key.as_str(), "omci_adapter" | "bootstrap_adapter")
                            && !key.starts_with("connection_request")
                            && !key.starts_with("auth_")
                    });
                }
                canonical_section(&mut business_canonical, id, &business);
            }
        }
    }
    cfg.revision = format!(
        "{:016x}",
        canonical
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64)
                .wrapping_mul(0x100000001b3))
    );
    cfg.business_revision = Some(format!(
        "{:016x}",
        business_canonical
            .bytes()
            .chain(crate::cwmp::registry::VERSION.to_le_bytes())
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64)
                .wrapping_mul(0x100000001b3))
    ));
    // Hash only compiled intent for this device. Host/listener settings and
    // another template's edits must not manufacture a new business generation.
    struct Fingerprint(u64);
    impl std::io::Write for Fingerprint {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for device in &cfg.devices {
        let mut hash = Fingerprint(0xcbf29ce484222325);
        serde_json::to_writer(
            &mut hash,
            &(
                crate::cwmp::registry::VERSION,
                &device.optical_sn,
                &device.cwmp_id,
                &device.provisioning_adapter,
                &device.wifi_cleanup,
                &device.wifi_preserve,
                &device.items,
            ),
        )
        .map_err(|_| "cannot fingerprint compiled device intent")?;
        cfg.device_revisions
            .insert(device.id.clone(), format!("{:016x}", hash.0));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fiberhome_count_and_pins_are_strict_and_redacted() {
        let base = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'TESTabcdef01'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n option group 'g'\n option onu_id '31'\n";
        assert_eq!(super::compile(base).unwrap().fiberhome_onu_count, None);
        for count in [32, 128] {
            let text = format!("{base}config fiberhome 'fiberhome'\n option onu_count '{count}'\n");
            assert_eq!(
                super::compile(&text).unwrap().fiberhome_onu_count,
                Some(count)
            );
        }
        let conflict = format!("{base}config fiberhome 'fiberhome'\n option onu_count '31'\n");
        let error = super::compile(&conflict).err().unwrap();
        assert!(!error.contains("TEST"));
        for value in ["0", "129", "", "-1", "+1", " 32"] {
            assert!(super::compile(&format!(
                "config fiberhome 'fiberhome'\n option onu_count '{value}'\n"
            ))
            .is_err());
        }
        for options in [
            "list onu_count '32'",
            "option onu_count '32'\n option onu_count '32'",
        ] {
            assert!(
                super::compile(&format!("config fiberhome 'fiberhome'\n {options}\n")).is_err()
            );
        }
        assert!(super::compile("config fiberhome 'other'\n option onu_count '32'\n").is_err());
    }

    #[test]
    fn fiberhome_rx_snapshot_accepts_legacy_and_boolean_options() {
        let base = "config fiberhome 'fiberhome'\n option onu_count '128'\n";
        let legacy = super::compile(base).unwrap();
        for value in ["0", "1", "false", "true"] {
            let updated =
                super::compile(&format!("{base} option rx_snapshot '{value}'\n")).unwrap();
            assert_eq!(updated.revision, legacy.revision);
        }
        for option in [
            "option rx_snapshot 'invalid'",
            "list rx_snapshot '1'",
            "option rx_snapshot '0'\n option rx_snapshot '1'",
        ] {
            assert!(super::compile(&format!("{base} {option}\n")).is_err());
        }
    }

    #[test]
    fn onu_pins_are_strict_unique_and_independent_of_business_revision() {
        let base = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'TESTabcdef01'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n option group 'g'\n";
        let automatic = super::compile(base).unwrap();
        for id in [0, 15, 16, 253] {
            let pinned = super::compile(&format!("{base} option onu_id '{id}'\n")).unwrap();
            assert_eq!(pinned.onu_pins["TESTABCDEF01"], id);
            assert_eq!(pinned.revision, automatic.revision);
        }
        for option in [
            "option onu_id '254'",
            "option onu_id '-1'",
            "option onu_id '+1'",
            "option onu_id '0x0f'",
            "option onu_id ''",
            "list onu_id '0'",
            "option onu_id ' 1'",
            "option onu_id '0'\n option onu_id '1'",
        ] {
            assert!(
                super::compile(&format!("{base}{option}\n")).is_err(),
                "{option}"
            );
        }
        let duplicate = format!("{base} option onu_id '0'\nconfig device 'other'\n option optical_sn 'TEST12345678'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n option group 'g'\n option onu_id '0'\n");
        assert!(super::compile(&duplicate)
            .err()
            .unwrap()
            .contains("already reserved"));
    }
    // Existing end-to-end expectations now cross the intent/compiler boundary
    // explicitly, using a known HL202/HWTC object-map fixture.
    fn plan(mut cfg: Config) -> Result<Config, String> {
        for device in &mut cfg.devices {
            assert!(device.items.iter().all(|i| i.parameters.is_empty()));
            let mut endpoints = crate::cwmp::path::EndpointMap::default();
            for n in 1..=4 {
                endpoints.0.insert(
                    super::super::model::EndpointRef::Lan(n),
                    format!("InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.{n}"),
                );
            }
            for radio in [2, 5] {
                for slot in 1..=4 {
                    endpoints.0.insert(
                        super::super::model::EndpointRef::Ssid { radio, slot },
                        format!(
                            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.{}",
                            if radio == 2 { slot } else { slot + 4 }
                        ),
                    );
                }
            }
            crate::cwmp::compiler::compile_device(device, &endpoints);
        }
        Ok(cfg)
    }
    fn compile(text: &str) -> Result<Config, String> {
        plan(super::compile(text)?)
    }

    #[test]
    fn packaged_defaults_accept_controller_optical_settings() {
        let defaults = include_str!("../../../files/fttr.config");
        let compiled = super::compile(defaults).unwrap();
        assert!(!compiled.mqtt_bridge_enabled);
        let disabled = defaults.replace(
            "config optical 'optical'\n\toption enabled '1'",
            "config optical 'optical'\n\toption enabled '0'",
        );
        assert!(super::compile(&disabled).is_ok());
        assert!(super::compile("config acs 'acs'\n option enabled '0'\n").is_ok());
    }

    #[test]
    fn mqtt_bridge_switch_is_strict_and_independent_of_business_revision() {
        let base = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT1234ABCD'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'hl202-mqtt'\n option group 'g'\n";
        let off = super::compile(base).unwrap();
        assert!(!off.mqtt_bridge_enabled);
        let on = super::compile(&format!(
            "{base}config mqtt_bridge 'mqtt_bridge'\n option enabled '1'\n"
        ))
        .unwrap();
        assert!(on.mqtt_bridge_enabled);
        assert_eq!(off.business_revision(), on.business_revision());
        assert_ne!(off.revision, on.revision);
        for bad in [
            "config mqtt_bridge 'other'\n option enabled '1'\n",
            "config mqtt_bridge 'mqtt_bridge'\n option enabled 'yes'\n",
            "config mqtt_bridge 'mqtt_bridge'\n list enabled '1'\n",
            "config mqtt_bridge 'mqtt_bridge'\n option extra '1'\n",
        ] {
            assert!(super::compile(&format!("{base}{bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn acs_bridge_and_redirect_rules_validate_cidr_port_path_and_device_and_keep_business_revision()
    {
        let base = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT1234ABCD'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\n";
        let off = super::compile(base).unwrap();
        assert!(!off.acs_bridge.enabled);
        let on = super::compile(&format!(
            "{base}config management 'management'\n option enabled '1'\n option mode 'auto'\n list vlan '46'\n option address '192.0.2.1/24'\nconfig acs_bridge 'acs_bridge'\n option enabled '1'\n option management_gateway '1'\nconfig acs_redirect 'r1'\n option scope 'lan'\n option target_ip '10.1.2.3/8'\n option target_port '9090'\n option path '*'\n option device 'H3CT1234ABCD'\nconfig acs_redirect 'r2'\n option scope 'management'\n option target_ip '10.20.30.40'\n option path '/tr069'\n"
        ))
        .unwrap();
        assert!(on.acs_bridge.enabled);
        assert!(on.acs_bridge.management_gateway);
        assert_eq!(
            on.management_network.as_ref().unwrap().management_gateway,
            cfg!(feature = "acs-bridge")
        );
        assert_eq!(on.acs_bridge.rules.len(), 2);
        assert_eq!(on.acs_bridge.rules[0].target_ip.to_string(), "10.0.0.0");
        assert_eq!(on.acs_bridge.rules[0].target_prefix, 8);
        assert_eq!(on.acs_bridge.rules[0].target_port, 9090);
        assert_eq!(
            on.acs_bridge.rules[0].path,
            crate::provisioning::model::AcsPathMatch::Wildcard
        );
        assert_eq!(
            on.acs_bridge.rules[1].path,
            crate::provisioning::model::AcsPathMatch::Exact("/tr069".into())
        );
        assert_eq!(off.business_revision(), on.business_revision());
        assert_ne!(off.revision, on.revision);

        for bad in [
            "config acs_bridge 'other'\n option enabled '1'\n",
            "config acs_bridge 'acs_bridge'\n option enabled 'yes'\n",
            "config acs_redirect 'bad'\n option target_ip '0.0.0.0/0'\n option path '*'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option target_port '0'\n option path '*'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option path 'tr069'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option path '/bad/../path'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option path '*'\n option scope 'wan'\n",
            "config acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option path '*'\n option device 'missing'\n",
            "config device 'dup'\n option optical_sn 'H3CT1234ABCD'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig acs_redirect 'bad'\n option target_ip '10.0.0.1'\n option path '*'\n option device 'H3CT1234ABCD'\n",
        ] {
            assert!(super::compile(&format!("{base}{bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn legacy_mqtt_switches_are_accepted_and_ignored() {
        let config = "config mqtt 'mqtt'\n option enabled '1'\n option hl202_enabled '1'\n option ty_fs803g_enabled '0'\n";
        let compiled = super::compile(config).unwrap();
        assert!(compiled.devices.is_empty());
        assert!(compiled.management.is_none());
    }

    #[test]
    fn optical_validation_matches_controller_contract() {
        for text in [
            "config optical 'other'",
            "config acs 'optical'",
            "config optical 'optical'\n option enabled 'yes'",
            "config optical 'optical'\n list enabled '1'",
            "config optical 'optical'\nconfig optical 'optical'",
            "config optical 'optical'\n option unknown '1'",
        ] {
            assert!(super::compile(text).is_err(), "{text}");
        }
    }

    #[test]
    fn management_vlan_list_is_strict_ordered_and_reserves_every_member() {
        let head = "config management 'management'\n option address '192.0.2.1/24'\n";
        for values in [
            "",
            " option vlan '46'\n",
            " list vlan '0'\n",
            " list vlan '4095'\n",
            " list vlan 'abc'\n",
            " list vlan '+46'\n",
            " list vlan '46'\n list vlan '046'\n",
        ] {
            assert!(
                super::compile(&format!("{head}{values}")).is_err(),
                "accepted {values:?}"
            );
        }
        let config = super::compile(&format!("{head} list vlan '47'\n list vlan '46'\n")).unwrap();
        let m = config.management.unwrap();
        assert_eq!(m.vlans, [47, 46]);
        assert_eq!(m.vlan, 47);
        assert_eq!(
            super::compile(&format!("{head} list vlan '46'\n"))
                .unwrap()
                .management
                .unwrap()
                .vlans,
            [46]
        );
        let business="config template 't'\nconfig network 'n'\n option template 't'\n option vlan '46'\n list service_list 'INTERNET'\n";
        let error = super::compile(&format!(
            "{head} list vlan '47'\n list vlan '46'\n{business}"
        ))
        .err()
        .unwrap();
        assert!(error.contains("conflicts with a business VLAN"));
    }

    #[test]
    #[cfg_attr(
        not(all(feature = "device-hwtc", feature = "acs")),
        ignore = "requires HWTC ACS adapter"
    )]
    fn configuration_outputs_only_business_intent_before_discovery() {
        let text="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'HWTC1234ABCD'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\n option group 'g'\nconfig network 'n'\n option template 't'\n option vlan '66'\n list service_list 'OTHER'\n list port 'lan2'\nconfig wifi 'w'\n option template 't'\n option ssid 'test'\n option radio '5g'";
        let cfg = super::compile(text).unwrap();
        assert!(cfg.devices[0]
            .items
            .iter()
            .all(|i| i.parameters.is_empty() && i.guard.is_none()));
        assert!(cfg.devices[0].items.iter().any(|i| i.wan_intent.is_some()));
        assert!(cfg.devices[0].items.iter().any(|i| i.wlan_intent.is_some()));
        assert!(cfg.devices[0].items.iter().any(|i| i
            .binding_intent
            .as_ref()
            .is_some_and(|b| b.endpoints == [super::super::model::EndpointRef::Lan(2)])));
    }

    #[test]
    fn global_bridge_fallback_policy_is_preserved() {
        let config =
            super::compile("config provisioning 'global'\n option allow_unsupported_as_bridge '1'")
                .unwrap();

        assert!(config.allow_unsupported_as_bridge);
    }

    use crate::provisioning::model::ItemKind;
    #[test]
    fn optical_policy_does_not_change_business_revision_and_legacy_fields_are_rejected() {
        let disabled =
            super::compile("config optical 'optical'\n option fec_enabled '0'\n").unwrap();
        let enabled = super::compile("config optical 'optical'\n option fec_enabled '1'\n option downstream_tx_enabled '0'\n").unwrap();
        assert_eq!(disabled.revision, enabled.revision);
        for source in ["auto", "factory", "firmware", "builtin"] {
            let selected = super::compile(&format!(
                "config calibration 'calibration'\n option source '{source}'\n"
            ))
            .unwrap();
            assert_eq!(disabled.revision, selected.revision);
        }
        assert!(
            super::compile("config calibration 'calibration'\n option source 'unknown'\n").is_err()
        );
        for key in [
            "fec_enabled",
            "downstream_tx_enabled",
            "downstream_fec_enabled",
        ] {
            assert!(super::compile(&format!(
                "config provisioning 'global'\n option {key} '1'\n"
            ))
            .is_err());
        }
    }

    #[test]
    fn bootstrap_selection_is_independent_per_device_and_changes_revision() {
        if !cfg!(all(feature = "all-devices", feature = "acs")) {
            return;
        }
        let config = |option: &str| {
            format!(
            "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h3c'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'generic'\n option provisioning_adapter 'hl202-cu'\n option bootstrap_adapter 'omci-acs'\n option group 'g'\nconfig device 'k155'\n option optical_sn 'HWTC12345678'\n option omci_adapter 'hwtc'\n option provisioning_adapter 'hwtc-hw'\n option bootstrap_adapter 'manual-acs'\n option group 'g'\n{option}"
        )
        };
        let default = compile(&config("")).unwrap();
        assert!(default
            .devices
            .iter()
            .find(|d| d.id == "h3c")
            .unwrap()
            .omci_management());
        assert!(!default
            .devices
            .iter()
            .find(|d| d.id == "k155")
            .unwrap()
            .omci_management());
        let explicit_off = compile(&config(" option bootstrap_adapter 'manual-acs'\n")).unwrap();
        assert!(!explicit_off
            .devices
            .iter()
            .find(|d| d.id == "k155")
            .unwrap()
            .omci_management());
        let enabled = compile(&config(" option bootstrap_adapter 'omci-acs'\n")).unwrap();
        assert!(enabled.devices.iter().all(|d| d.omci_management()));
        assert_ne!(explicit_off.revision, enabled.revision);
        assert_eq!(
            explicit_off.business_revision(),
            enabled.business_revision()
        );
        assert!(compile(&config(" option bootstrap_adapter 'invalid'\n"))
            .err()
            .unwrap()
            .contains("bootstrap adapter"));
    }
    #[test]
    fn bootstrap_and_business_choices_are_independent_and_only_business_changes_schedule_sync() {
        for bootstrap in crate::adapters::bootstrap::available() {
            for provisioning in crate::adapters::provisioning::available() {
                let text = format!("config management 'management'\n option address '192.0.2.1/24'\n list vlan '46'\nconfig template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT1234ABCD'\n option group 'g'\n option omci_adapter 'generic'\n option bootstrap_adapter '{}'\n option provisioning_adapter '{}'\n", bootstrap.id, provisioning.id);
                let config = super::compile(&text).unwrap();
                assert_eq!(config.devices[0].bootstrap_adapter, bootstrap.id);
                assert_eq!(config.devices[0].provisioning_adapter, provisioning.id);
                let host = config.management_network.as_ref().unwrap();
                assert_eq!(host.devices.contains_key("d"), bootstrap.id != "none");
                let no_bootstrap = super::compile(&text.replace(
                    &format!("option bootstrap_adapter '{}'", bootstrap.id),
                    "option bootstrap_adapter 'none'",
                ))
                .unwrap();
                assert_eq!(config.business_revision(), no_bootstrap.business_revision());
                let disabled = super::compile(&text.replace(
                    &format!("option provisioning_adapter '{}'", provisioning.id),
                    "option provisioning_adapter 'none'",
                ))
                .unwrap();
                if provisioning.id != "none" {
                    assert_ne!(config.business_revision(), disabled.business_revision());
                }
            }
        }
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "acs")),
        ignore = "requires HL202 ACS adapter"
    )]
    fn hl202_internet_bridge_dhcp_and_second_radio_slots_compile_for_binding() {
        let text = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig wifi 'a2'\n option template 't'\n option ssid 'base2'\n option radio '2g'\nconfig wifi 'a5'\n option template 't'\n option ssid 'base5'\n option radio '5g'\nconfig wifi 'b2'\n option template 't'\n option ssid 'TTTT'\n option radio '2g'\nconfig wifi 'b5'\n option template 't'\n option ssid 'YYYY'\n option radio '5g'\nconfig network 'iot'\n option template 't'\n option vlan '66'\n list service_list 'INTERNET'\n option priority '6'\n list ssid 'b2'\n list ssid 'b5'";
        let cfg = compile(text).unwrap();
        let items = &cfg.devices[0].items;
        let service = items
            .iter()
            .find(|i| i.kind == ItemKind::DeviceService)
            .unwrap();
        for (suffix, value) in [
            (".ConnectionType", "IP_Bridged"),
            (".NATEnabled", "0"),
            (".X_CU_CloudCPEEnable", "1"),
        ] {
            assert!(service
                .parameters
                .iter()
                .any(|p| p.name.ends_with(suffix) && p.value == value));
        }
        let binding = items
            .iter()
            .find(|i| i.kind == ItemKind::PortBinding)
            .unwrap();
        assert!(binding.blocked.is_none());
        assert_eq!(binding.parameters[0].value, "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2,InternetGatewayDevice.LANDevice.1.WLANConfiguration.6");
    }
    #[test]
    fn custom_vlan_service_list_supports_multiple_tokens() {
        if !cfg!(all(feature = "device-hl202", feature = "acs")) {
            return;
        }
        let text = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig network 'n'\n option template 't'\n option vlan '123'\n list service_list 'iptv,VOIP'\n list service_list 'OTHER'";
        let cfg = compile(text).unwrap();
        let item = cfg.devices[0]
            .items
            .iter()
            .find(|item| item.kind == ItemKind::DeviceService)
            .unwrap();
        assert!(item
            .parameters
            .iter()
            .any(|p| { p.name.ends_with("X_CU_ServiceList") && p.value == "IPTV,VOIP,OTHER" }));
    }
    #[test]
    fn service_list_infers_internet_behavior() {
        if !cfg!(all(feature = "device-hl202", feature = "acs")) {
            return;
        }
        let head = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\n";
        let internet = format!(
            "{head}config network 'n'\n option template 't'\n option vlan '123'\n list service_list 'INTERNET,IPTV'"
        );
        let cfg = compile(&internet).unwrap();
        let service = cfg.devices[0]
            .items
            .iter()
            .find(|item| item.kind == ItemKind::DeviceService)
            .unwrap();
        assert_eq!(service.service_type, "internet");
        assert!(service.parameters.iter().any(|parameter| {
            parameter.name.ends_with("X_CU_ServiceList") && parameter.value == "INTERNET,IPTV"
        }));

        let other = format!(
            "{head}config network 'n'\n option template 't'\n option vlan '123'\n list service_list 'OTHER,VOIP'"
        );
        let cfg = compile(&other).unwrap();
        let service = cfg.devices[0]
            .items
            .iter()
            .find(|item| item.kind == ItemKind::DeviceService)
            .unwrap();
        assert_eq!(service.service_type, "iptv");
        assert!(!service
            .parameters
            .iter()
            .any(|parameter| parameter.name.ends_with("X_CU_CloudCPEEnable")));
    }
    #[test]
    fn custom_vlan_service_list_rejects_tr069_and_untagged_internet_is_not_managed() {
        let head = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT1234ABCD'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\n";
        let tr069 = format!("{head}config network 'n'\n option template 't'\n option vlan '123'\n list service_list 'IPTV,TR069'");
        assert!(compile(&tr069).err().unwrap().contains("TR069"));
        let untagged = format!("{head}config network 'n'\n option template 't'\n option vlan '0'\n list service_list 'INTERNET'");
        assert!(compile(&untagged)
            .err()
            .unwrap()
            .contains("untagged INTERNET"));
        let missing = format!("{head}config network 'n'\n option template 't'\n option vlan '123'");
        assert!(compile(&missing)
            .err()
            .unwrap()
            .contains("select at least one WAN Service List label"));
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "device-hwtc", feature = "acs")),
        ignore = "requires both device contracts"
    )]
    fn wpa2_uses_device_beacon_contract() {
        let text="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig device 'k'\n option optical_sn 'HWTC12345678'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'test'\n option radio '2g'\n option security 'wpa2-psk'\n option key 'testpassword'";
        let cfg = compile(text).unwrap();
        for d in &cfg.devices {
            let beacon = d.items[0]
                .parameters
                .iter()
                .find(|p| p.name.ends_with(".BeaconType"))
                .unwrap();
            assert_eq!(
                beacon.value,
                if d.provisioning_adapter == "hl202-cu" {
                    "WPA2"
                } else {
                    "11i"
                }
            );
        }
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "device-hwtc", feature = "acs")),
        ignore = "requires both device contracts"
    )]
    fn wifi_security_modes_compile_expected_parameters() {
        // 1. None (Open)
        let text_none = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'open_net'\n option radio '2g'\n option security 'none'";
        let cfg_none = compile(text_none).unwrap();
        let p_none = &cfg_none.devices[0].items[0].parameters;
        assert_eq!(
            p_none
                .iter()
                .find(|p| p.name.ends_with(".BeaconType"))
                .unwrap()
                .value,
            "None"
        );
        assert_eq!(
            p_none
                .iter()
                .find(|p| p.name.ends_with(".BasicEncryptionModes"))
                .unwrap()
                .value,
            "None"
        );
        assert_eq!(
            p_none
                .iter()
                .find(|p| p.name.ends_with(".BasicAuthenticationMode"))
                .unwrap()
                .value,
            "None"
        );
        assert!(p_none.iter().all(|p| !p.name.contains("PreSharedKey")));

        // 2. WPA3-SAE
        let text_wpa3 = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig device 'k'\n option optical_sn 'HWTC12345678'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'wpa3_net'\n option radio '5g'\n option security 'wpa3-sae'\n option key 'pass123456'";
        let cfg_wpa3 = compile(text_wpa3).unwrap();
        for d in &cfg_wpa3.devices {
            let p = &d.items[0].parameters;
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".BeaconType"))
                    .unwrap()
                    .value,
                "WPA3"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".IEEE11iEncryptionModes"))
                    .unwrap()
                    .value,
                "AESEncryption"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".IEEE11iAuthenticationMode"))
                    .unwrap()
                    .value,
                "PSKAuthentication"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".KeyPassphrase"))
                    .unwrap()
                    .value,
                "pass123456"
            );
        }

        // 3. WPA2-PSK / WPA3-SAE mixed
        let text_wpa2_wpa3 = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig device 'k'\n option optical_sn 'HWTC12345678'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'mixed_net'\n option radio '2g'\n option security 'wpa2-psk/wpa3-sae'\n option key 'pass123456'";
        let cfg_wpa2_wpa3 = compile(text_wpa2_wpa3).unwrap();
        for d in &cfg_wpa2_wpa3.devices {
            let p = &d.items[0].parameters;
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".BeaconType"))
                    .unwrap()
                    .value,
                "WPA2/WPA3"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".IEEE11iEncryptionModes"))
                    .unwrap()
                    .value,
                "AESEncryption"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".KeyPassphrase"))
                    .unwrap()
                    .value,
                "pass123456"
            );
        }

        // 4. WPA-PSK / WPA2-PSK mixed
        let text_wpa_wpa2 = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig device 'k'\n option optical_sn 'HWTC12345678'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'legacy_net'\n option radio '2g'\n option security 'wpa-psk/wpa2-psk'\n option key 'pass123456'";
        let cfg_wpa_wpa2 = compile(text_wpa_wpa2).unwrap();
        for d in &cfg_wpa_wpa2.devices {
            let p = &d.items[0].parameters;
            let expected_beacon = if d.provisioning_adapter == "hl202-cu" {
                "WPA/WPA2"
            } else {
                "WPAand11i"
            };
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".BeaconType"))
                    .unwrap()
                    .value,
                expected_beacon
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".WPAEncryptionModes"))
                    .unwrap()
                    .value,
                "TKIPandAESEncryption"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".WPAAuthenticationMode"))
                    .unwrap()
                    .value,
                "PSKAuthentication"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".IEEE11iEncryptionModes"))
                    .unwrap()
                    .value,
                "TKIPandAESEncryption"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".IEEE11iAuthenticationMode"))
                    .unwrap()
                    .value,
                "PSKAuthentication"
            );
            assert_eq!(
                p.iter()
                    .find(|p| p.name.ends_with(".KeyPassphrase"))
                    .unwrap()
                    .value,
                "pass123456"
            );
        }

        // 5. Keep current key (empty security)
        let text_keep = "config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'h'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig wifi 'w'\n option template 't'\n option ssid 'keep_net'\n option radio '2g'\n option security ''";
        let cfg_keep = compile(text_keep).unwrap();
        let p_keep = &cfg_keep.devices[0].items[0].parameters;
        assert!(p_keep
            .iter()
            .all(|p| !p.name.contains("BeaconType") && !p.name.contains("KeyPassphrase")));
    }
    #[test]
    fn rejects_legacy_and_acs_management() {
        assert!(compile("config device_service 'old'").is_err());
        assert!(compile("config template 't'\nconfig network 'n'\n option template 't'\n option type 'tr069'\n option vlan '46'").err().unwrap().contains("unsupported option type"));
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hwtc", feature = "acs")),
        ignore = "requires HWTC ACS adapter"
    )]
    fn enabled_ssid_requires_radio_but_disabled_ssid_does_not_disable_shared_radio() {
        let text="config device 'd'\n option optical_sn 'HWTC1234ABCD'\n option cwmp_id 'test:hwtc'\n option omci_adapter 'hwtc'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hwtc-hw'\nconfig wifi_profile 'a'\n option ssid 'enabled'\n option enabled '1'\nconfig wifi_profile 'b'\n option ssid 'disabled'\n option enabled '0'\nconfig ssid 'a'\n option device 'd'\n option profile 'a'\n option radio '2g'\n option slot '1'\nconfig ssid 'b'\n option device 'd'\n option profile 'b'\n option radio '2g'\n option slot '2'";
        let text = text
            .replace("config ssid 'a'", "config ssid 'ssid_a'")
            .replace("config ssid 'b'", "config ssid 'ssid_b'");
        let cfg = plan(config::parse_expanded(&text).unwrap()).unwrap();
        let items = &cfg.devices[0].items;
        assert!(items[0]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".RadioEnabled") && p.value == "1"));
        assert!(!items[1]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".RadioEnabled")));
        assert!(items[1]
            .parameters
            .iter()
            .any(|p| p.name.ends_with(".Enable") && p.value == "0"));
    }
    #[test]
    fn rejects_cross_template_binding() {
        assert!(compile("config template 'a'\nconfig template 'b'\nconfig wifi 'w'\n option template 'b'\n option ssid 'x'\n option radio '2g'\nconfig network 'n'\n option template 'a'\n option vlan '1'\n list service_list 'INTERNET'\n list ssid 'w'").is_err());
    }
    #[test]
    fn creation_order_changes_revision() {
        assert_ne!(
            compile("config template 'z'\nconfig template 'a'")
                .unwrap()
                .revision,
            compile("config template 'a'\nconfig template 'z'")
                .unwrap()
                .revision
        );
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "acs")),
        ignore = "requires HL202 ACS adapter"
    )]
    fn wlan_cleanup_defaults_to_disable_and_explicit_policy_is_preserved() {
        let base="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'";
        assert_eq!(compile(base).unwrap().devices[0].wifi_cleanup, "disable");
        for policy in ["preserve", "disable", "delete"] {
            let text = format!("{base}\n option wifi_cleanup '{policy}'");
            assert_eq!(compile(&text).unwrap().devices[0].wifi_cleanup, policy);
        }
        assert!(compile(&format!("{base}\n option wifi_cleanup 'remove-everything'")).is_err());
    }
    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "acs")),
        ignore = "requires HL202 ACS adapter"
    )]
    fn group_expansion_keeps_creation_order_and_no_public_indices() {
        let text="config template 't'\nconfig group 'g'\n option template 't'\nconfig device 'd'\n option optical_sn 'H3CT12345678'\n option omci_adapter 'hl202'\n option bootstrap_adapter 'manual-acs'\n option provisioning_adapter 'hl202-cu'\n option group 'g'\nconfig network 'z'\n option template 't'\n option vlan '100'\n list service_list 'INTERNET'\nconfig network 'a'\n option template 't'\n option vlan '43'\n list service_list 'OTHER'\nconfig wifi 'w5'\n option template 't'\n option ssid 'five'\n option radio '5g'\nconfig wifi 'w2'\n option template 't'\n option ssid 'two'\n option radio '2g'";
        let cfg = compile(text).unwrap();
        let d = &cfg.devices[0];
        assert_eq!(
            d.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            [
                "d__network__z",
                "d__network__a",
                "d__wifi__w5",
                "d__wifi__w2"
            ]
        );
        assert!(d.items[..2].iter().all(|i| i.auto_instance.is_some()));
        assert!(d.items[2].parameters[0]
            .name
            .contains("WLANConfiguration.5."));
        assert_eq!(cfg.assignments["d"], ("g".into(), "t".into()));
        assert!(compile(&format!("{text}\n option instance '1'")).is_err());
    }
    #[test]
    fn one_endpoint_cannot_belong_to_two_networks() {
        assert!(compile("config template 't'\nconfig network 'a'\n option template 't'\n option vlan '1'\n list service_list 'INTERNET'\n list port 'lan1'\nconfig network 'b'\n option template 't'\n option vlan '2'\n list service_list 'IPTV'\n list port 'lan1'").is_err());
    }
}
