use super::model::ItemKind;
pub(super) use super::model::{Config, Device, Item, Management, Parameter};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const CUSTOM_VLAN_SERVICE_LIST_ALLOWED: &[&str] = &["INTERNET", "OTHER", "IPTV", "VOIP"];

#[derive(Clone, Default)]
pub struct Section {
    pub kind: String,
    pub options: BTreeMap<String, Vec<String>>,
}
impl Section {
    pub fn get(&self, key: &str) -> &str {
        self.options
            .get(key)
            .and_then(|v| v.first())
            .map(String::as_str)
            .unwrap_or("")
    }
    pub fn number(&self, key: &str, max: u32) -> Result<u32, String> {
        self.get(key)
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= max)
            .ok_or_else(|| format!("invalid {key}"))
    }
}

pub(super) fn normalize_service_list(values: &[String]) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    for value in values {
        for token in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            let token = token.to_ascii_uppercase();
            if token == "TR069" {
                return Err("TR069 service labels are managed by OMCI".into());
            }
            if !CUSTOM_VLAN_SERVICE_LIST_ALLOWED.contains(&token.as_str()) {
                return Err(format!("unsupported service_list token: {token}"));
            }
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    Ok(tokens)
}

pub(super) fn normalize_custom_vlan_service_list(values: &[String]) -> Result<Vec<String>, String> {
    let tokens = normalize_service_list(values)?;
    if let Some(token) = tokens
        .iter()
        .find(|token| !CUSTOM_VLAN_SERVICE_LIST_ALLOWED.contains(&token.as_str()))
    {
        return Err(format!("{token} is not allowed on a custom VLAN"));
    }
    Ok(tokens)
}

pub fn load() -> Result<Config, String> {
    super::templates::compile_view(&crate::config::load()?)
}
pub fn parse(text: &str) -> Result<Config, String> {
    super::templates::compile(text)
}
/// Decode UCI once at the input boundary; compilers pass sections directly.
pub(super) fn parse_sections(text: &str) -> Result<Vec<(String, Section)>, String> {
    sections_from_view(&crate::config::View::parse(text)?)
}
pub(super) fn sections_from_view(
    view: &crate::config::View,
) -> Result<Vec<(String, Section)>, String> {
    let mut sections = Vec::<(String, Section)>::new();
    for input in &view.sections {
        let id = input
            .name
            .clone()
            .unwrap_or_else(|| format!("_anon_{}_{}", input.kind, sections.len()));
        if sections.iter().any(|(existing, _)| existing == &id) {
            return Err("duplicate section".into());
        }
        sections.push((
            id,
            Section {
                kind: input.kind.clone(),
                ..Section::default()
            },
        ));
        for option in &input.options {
            let section = &mut sections.last_mut().ok_or("option without section")?.1;
            let key = &option.key;
            if (section.kind == "omci_auth" || section.kind == "device")
                && (key.starts_with("auth_") || key == "schema_version")
                && (option.is_list || section.options.contains_key(key))
            {
                return Err("OMCI authentication fields require unique scalar options".into());
            }
            if section.kind == "device"
                && key == "onu_id"
                && (option.is_list || section.options.contains_key("onu_id"))
            {
                return Err("device onu_id requires one scalar option".into());
            }
            if section.kind == "fiberhome"
                && matches!(key.as_str(), "onu_count" | "rx_snapshot")
                && (option.is_list || section.options.contains_key(key))
            {
                return Err(format!("fiberhome {} requires one scalar option", key));
            }
            if section.kind == "mqtt_bridge"
                && key == "enabled"
                && (option.is_list || section.options.contains_key("enabled"))
            {
                return Err("mqtt_bridge enabled requires one scalar option".into());
            }
            if section.kind == "acs_bridge"
                && matches!(key.as_str(), "enabled" | "management_gateway")
                && (option.is_list || section.options.contains_key(key))
            {
                return Err(format!("acs_bridge {key} requires one scalar option"));
            }
            if section.kind == "acs_redirect"
                && matches!(
                    key.as_str(),
                    "enabled" | "scope" | "target_ip" | "target_port" | "path" | "device" | "name"
                )
                && (option.is_list || section.options.contains_key(key))
            {
                return Err(format!("acs_redirect {key} requires one scalar option"));
            }
            if section.kind == "management" && key == "vlan" && !option.is_list {
                return Err(
                    "management VLAN requires list vlan; scalar option vlan is unsupported".into(),
                );
            }
            if !option.is_list {
                section
                    .options
                    .insert(key.clone(), vec![option.value.clone()]);
            } else {
                section
                    .options
                    .entry(key.clone())
                    .or_default()
                    .push(option.value.clone());
            }
        }
    }
    Ok(sections)
}

#[cfg(test)]
pub(super) fn parse_expanded(text: &str) -> Result<Config, String> {
    compile_sections(parse_sections(text)?)
}

pub(super) fn compile_sections(input: Vec<(String, Section)>) -> Result<Config, String> {
    let mut sections = BTreeMap::new();
    for (id, section) in input {
        if sections.insert(id, section).is_some() {
            return Err("duplicate section".into());
        }
    }
    let globals: Vec<_> = sections
        .values()
        .filter(|s| s.kind == "provisioning")
        .collect();
    if globals.len() > 1 {
        return Err("only one provisioning section allowed".into());
    }
    let mut cfg = Config {
        freshness: 180,
        allow_unsupported_as_bridge: true,
        ..Config::default()
    };
    cfg.omci_auth = compile_auth(&sections)?;
    if let Some(s) = globals.first() {
        cfg.enabled = boolean(s.get("enabled"), false)?;
        cfg.allow_unsupported_as_bridge = boolean(s.get("allow_unsupported_as_bridge"), true)?;
        cfg.automatic = match s.get("mode") {
            "" | "manual" => false,
            "auto" => true,
            _ => return Err("mode must be manual or auto".into()),
        };
        if !matches!(s.get("method"), "" | "diff") {
            return Err("only diff synchronization is supported".into());
        }
        cfg.interval_seconds = if s.get("interval_minutes").is_empty() {
            0
        } else {
            u64::from(s.number("interval_minutes", 1440)?) * 60
        };
        cfg.freshness = if s.get("freshness").is_empty() {
            180
        } else {
            s.number("freshness", 86400)? as u64
        };
        if cfg.freshness == 0 {
            return Err("freshness must be positive".into());
        }
        cfg.triggers = s
            .options
            .get("trigger")
            .cloned()
            .unwrap_or_else(|| {
                vec![
                    "config_change".into(),
                    "device_online".into(),
                    "cwmp_boot".into(),
                    "program_start".into(),
                    "periodic".into(),
                ]
            })
            .into_iter()
            .collect();
        if cfg.triggers.iter().any(|t| {
            !matches!(
                t.as_str(),
                "config_change" | "device_online" | "cwmp_boot" | "periodic" | "program_start"
            )
        }) {
            return Err("unknown synchronization trigger".into());
        }
    }
    let mut sns = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for (id, s) in sections.iter().filter(|(_, s)| s.kind == "device") {
        if s.get("optical_sn").len() != 12
            || !s.get("optical_sn").is_ascii()
            || !s.get("optical_sn")[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(format!("{id}: invalid optical_sn"));
        }
        if s.get("cwmp_id").is_empty()
            || !sns.insert(s.get("optical_sn").to_ascii_uppercase())
            || !ids.insert(s.get("cwmp_id"))
        {
            return Err(format!("{id}: missing/duplicate device identity"));
        }
        if let Some(values) = s.options.get("onu_id") {
            let value = s.get("onu_id");
            if values.len() != 1 || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "{id}: onu_id must be a single decimal number {}..{}",
                    crate::pon_engine::onu_id::MIN_ONU_ID,
                    crate::pon_engine::onu_id::MAX_ONU_ID
                ));
            }
            let slot = value
                .parse::<u8>()
                .ok()
                .filter(|n| crate::pon_engine::onu_id::is_valid(u16::from(*n)))
                .ok_or_else(|| {
                    format!(
                        "{id}: onu_id must be {}..{}",
                        crate::pon_engine::onu_id::MIN_ONU_ID,
                        crate::pon_engine::onu_id::MAX_ONU_ID
                    )
                })?;
            if cfg.onu_pins.values().any(|n| *n == slot) {
                return Err(format!("{id}: onu_id {slot} already reserved"));
            }
            cfg.onu_pins
                .insert(s.get("optical_sn").to_ascii_uppercase(), slot);
        }
        let omci_adapter = match s.get("omci_adapter") {
            "" => "auto",
            other => other,
        };
        crate::adapters::omci::validate(omci_adapter).map_err(|e| format!("{id}: {e}"))?;
        let provisioning_adapter = s.get("provisioning_adapter");
        crate::adapters::provisioning::lookup(provisioning_adapter)
            .map_err(|e| format!("{id}: {e}"))?;
        let bootstrap_adapter = s.get("bootstrap_adapter");
        crate::adapters::bootstrap::validate(bootstrap_adapter)
            .map_err(|e| format!("{id}: {e}"))?;
        let wifi_cleanup = match s.get("wifi_cleanup") {
            "" | "preserve" => "preserve",
            "delete" => "delete",
            "disable" => "disable",
            _ => {
                return Err(format!(
                    "{id}: wifi_cleanup must be preserve, delete or disable"
                ))
            }
        };
        let wifi_preserve = s
            .options
            .get("wifi_preserve")
            .into_iter()
            .flatten()
            .map(|v| {
                v.parse::<u32>()
                    .ok()
                    .filter(|n| (1..=32).contains(n))
                    .ok_or("invalid wifi_preserve instance")
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let use_defaults = boolean(
            s.get("connection_request_defaults"),
            s.get("connection_request_username").is_empty()
                || s.get("connection_request_password").is_empty(),
        )
        .map_err(|error| format!("{id}: connection_request_defaults: {error}"))?;
        let port = if s.get("connection_request_port").is_empty() {
            super::model::ConnectionRequest::default().port
        } else {
            s.get("connection_request_port")
                .parse::<std::num::NonZeroU16>()
                .map_err(|_| format!("{id}: connection_request_port must be 1..65535"))?
        };
        let connection_request = super::model::ConnectionRequest {
            enabled: boolean(s.get("connection_request"), true)
                .map_err(|error| format!("{id}: connection_request: {error}"))?,
            defaults: use_defaults,
            username: s.get("connection_request_username").into(),
            password: s.get("connection_request_password").into(),
            port,
            ..Default::default()
        };
        cfg.devices.push(Device {
            id: id.clone(),
            optical_sn: s.get("optical_sn").to_ascii_uppercase(),
            cwmp_id: s.get("cwmp_id").into(),
            omci_adapter: omci_adapter.into(),
            bootstrap_adapter: bootstrap_adapter.into(),
            provisioning_adapter: provisioning_adapter.into(),
            resolved_contract: None,
            connection_request,
            items: Vec::new(),
            wifi_cleanup: wifi_cleanup.into(),
            wifi_preserve,
        });
    }
    for (id, s) in &sections {
        let kind = match s.kind.as_str() {
            "device_service" => ItemKind::DeviceService,
            "ssid" => ItemKind::Ssid,
            "port_binding" => ItemKind::PortBinding,
            _ => continue,
        };
        let d = cfg
            .devices
            .iter_mut()
            .find(|d| d.id == s.get("device"))
            .ok_or_else(|| format!("{id}: unknown device"))?;
        let mut item = Item {
            wlan_intent: None,
            binding_intent: None,
            id: id.clone(),
            kind,
            service_type: match kind {
                ItemKind::Ssid => "wifi".into(),
                _ => String::new(),
            },
            parameters: Vec::new(),
            guard: None,
            blocked: None,
            wan_ref: None,
            auto_instance: None,
            wan_intent: None,
        };
        if s.kind == "ssid" {
            let profile = reference(&sections, s.get("profile"), "wifi_profile")?;
            if profile.get("ssid").is_empty() || profile.get("ssid").len() > 32 {
                return Err(format!("{id}: SSID must be 1..32 bytes"));
            }
            let slot = s.number("slot", 4)?;
            if slot == 0 {
                return Err("slot must be 1..4".into());
            }
            let base = match s.get("radio") {
                "2g" => 0,
                "5g" => 4,
                _ => return Err("radio must be 2g or 5g".into()),
            };
            if !s.get("instance").is_empty() {
                return Err(
                    "WLAN instance overrides were removed; use a logical radio/slot".into(),
                );
            }
            let instance = base + slot;
            if instance == 0 {
                return Err("invalid WLAN instance".into());
            }
            let endpoint = super::model::EndpointRef::Ssid {
                radio: if base == 0 { 2 } else { 5 },
                slot: (instance - base),
            };
            let security = match profile.get("security") {
                "" => None,
                "none" => Some(super::model::WifiSecurity::None),
                "wpa2-psk" => Some(super::model::WifiSecurity::Wpa2Psk),
                "wpa3-sae" => Some(super::model::WifiSecurity::Wpa3Sae),
                "wpa2-psk/wpa3-sae" => Some(super::model::WifiSecurity::Wpa2PskWpa3Sae),
                "wpa-psk/wpa2-psk" => Some(super::model::WifiSecurity::WpaPskWpa2Psk),
                _ => return Err("invalid Wi-Fi security".into()),
            };
            let key = match security {
                Some(super::model::WifiSecurity::None) | None => None,
                Some(_) => {
                    let k = profile.get("key");
                    if !(8..=63).contains(&k.len()) {
                        return Err("Wi-Fi key must be 8..63 bytes".into());
                    }
                    Some(k.to_owned())
                }
            };
            let intent = super::model::WlanIntent {
                endpoint,
                ssid: profile.get("ssid").into(),
                enabled: (!profile.get("enabled").is_empty())
                    .then(|| boolean(profile.get("enabled"), true))
                    .transpose()?,
                broadcast: (!profile.get("broadcast").is_empty())
                    .then(|| boolean(profile.get("broadcast"), true))
                    .transpose()?,
                security,
                key,
            };
            item.wlan_intent = Some(intent);
            if !s.get("device_service").is_empty() {
                let ds = reference(&sections, s.get("device_service"), "device_service")?;
                if ds.get("device") != d.id {
                    return Err("cross-device SSID service reference".into());
                }
                if !sections.values().any(|b| {
                    b.kind == "port_binding"
                        && b.get("device_service") == s.get("device_service")
                        && b.options.get("interface").is_some_and(|v| {
                            v.contains(&format!("ssid:{}:{}", if base == 0 { 2 } else { 5 }, slot))
                        })
                }) {
                    return Err(format!("{id}: device_service requires explicit port_binding interface for this WLAN object"));
                }
            }
        } else {
            let ds = if s.kind == "device_service" {
                s
            } else {
                reference(&sections, s.get("device_service"), "device_service")?
            };
            if ds.get("device") != d.id {
                return Err(format!("{id}: cross-device binding"));
            }
            let auto = matches!(ds.get("wan_instance"), "" | "auto");
            if !auto {
                return Err(
                    "WAN instance overrides were removed; instances are resolved from inventory"
                        .into(),
                );
            }
            let instance = if auto {
                1000 + sections
                    .keys()
                    .position(|key| {
                        key == if s.kind == "device_service" {
                            id
                        } else {
                            s.get("device_service")
                        }
                    })
                    .unwrap() as u32
            } else {
                ds.number("wan_instance", 64)?
            };
            if instance == 0 {
                return Err("wan_instance must be positive".into());
            }
            item.wan_ref = Some(if s.kind == "device_service" {
                id.clone()
            } else {
                s.get("device_service").into()
            });
            if auto {
                item.auto_instance = Some(instance);
            }
            let service = reference(&sections, ds.get("service"), "service")?;
            item.service_type = match service.get("type") {
                "internet" => "internet",
                "iptv" => "iptv",
                _ => return Err("business type must be internet or iptv".into()),
            }
            .into();
            if s.kind == "port_binding" {
                if item.service_type == "tr069" {
                    return Err("TR069 management WAN cannot have business port bindings".into());
                }
                let interfaces = s
                    .options
                    .get("interface")
                    .ok_or("port_binding requires logical endpoints")?;
                let endpoints = interfaces
                    .iter()
                    .map(|v| crate::cwmp::path::parse_endpoint(v))
                    .collect::<Result<Vec<_>, _>>()?;
                item.binding_intent = Some(super::model::BindingIntent { endpoints });
            } else {
                let service_list_values =
                    ds.options.get("service_list").cloned().unwrap_or_default();
                let service_list = normalize_service_list(&service_list_values)?.join(",");
                if auto && service_list.is_empty() {
                    return Err(format!(
                        "{id}: automatic WAN requires explicit vendor service_list"
                    ));
                }
                if super::contains_service_token(&service_list, "TR069") {
                    return Err(format!("{id}: TR069 service_list is managed by OMCI"));
                }
                let vlan = service.number("vlan", 4094)?;
                if item.service_type == "internet" && vlan == 0 {
                    return Err(format!(
                        "{id}: untagged INTERNET WAN is not managed by ACS automation"
                    ));
                }
                let priority = if service.get("priority").is_empty() {
                    0
                } else {
                    service.number("priority", 7)?
                };
                let intent = super::model::WanIntent {
                    services: normalize_service_list(&service_list_values)?,
                    vlan: if vlan == 0 {
                        super::model::VlanMode::Untagged
                    } else {
                        super::model::VlanMode::Tagged(vlan as u16)
                    },
                    priority: priority as u8,
                    mode: super::model::ConnectionMode::Bridge,
                    bindings: Vec::new(),
                    internet: item.service_type == "internet",
                    enabled: boolean(ds.get("enabled"), true)?,
                };
                item.wan_intent = Some(intent);
            }
        }
        d.items.push(item);
    }
    for d in &mut cfg.devices {
        d.items.sort_by_key(|i| {
            (
                if i.service_type == "tr069" {
                    3
                } else {
                    match i.kind {
                        ItemKind::DeviceService => 0,
                        ItemKind::Ssid => 1,
                        ItemKind::PortBinding | ItemKind::WanActivation => 2,
                    }
                },
                i.id.clone(),
            )
        });
        let mut paths = BTreeSet::new();
        for i in &d.items {
            for p in &i.parameters {
                if !paths.insert(&p.name) {
                    return Err(format!("{}: duplicate parameter ownership", d.id));
                }
            }
        }
    }
    // Stable revision over provisioning sections only; ACS credentials never enter it.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in crate::cwmp::registry::VERSION.to_le_bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    for (id, s) in sections.iter().filter(|(_, s)| {
        matches!(
            s.kind.as_str(),
            "provisioning"
                | "device"
                | "service"
                | "device_service"
                | "wifi_profile"
                | "ssid"
                | "port_binding"
        )
    }) {
        for byte in format!("{id}\0{}\0{:?}", s.kind, s.options).bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }
    cfg.revision = format!("{hash:016x}");
    Ok(cfg)
}

pub(super) fn compile_auth(
    sections: &BTreeMap<String, Section>,
) -> Result<crate::omci_auth::Config, String> {
    let mut auth = crate::omci_auth::Config::default();
    let globals: Vec<_> = sections
        .iter()
        .filter(|(_, s)| s.kind == "omci_auth")
        .collect();
    if globals.len() > 1
        || globals
            .first()
            .is_some_and(|(id, _)| !matches!(id.as_str(), "global" | "omci_auth_global"))
    {
        return Err("OMCI authentication requires one named global section".into());
    }
    if let Some((_, s)) = globals.first() {
        if s.get("schema_version") != "1" {
            return Err("OMCI authentication schema_version must be 1".into());
        }
        auth.global = crate::omci_auth::parse_policy(
            s.get("auth_mode"),
            s.get("auth_loid"),
            s.get("auth_password"),
            false,
        )?
        .unwrap();
    }
    for s in sections.values().filter(|s| s.kind == "device") {
        auth.selections.insert(
            s.get("optical_sn").to_ascii_uppercase(),
            s.get("omci_adapter").into(),
        );
        if let Some(policy) = crate::omci_auth::parse_policy(
            s.get("auth_mode"),
            s.get("auth_loid"),
            s.get("auth_password"),
            true,
        )? {
            auth.devices
                .insert(s.get("optical_sn").to_ascii_uppercase(), policy);
        }
    }
    Ok(auth)
}
fn reference<'a>(
    sections: &'a BTreeMap<String, Section>,
    id: &str,
    kind: &str,
) -> Result<&'a Section, String> {
    sections
        .get(id)
        .filter(|s| s.kind == kind)
        .ok_or_else(|| format!("missing {kind} reference {id}"))
}
fn boolean(value: &str, default: bool) -> Result<bool, String> {
    match value {
        "" => Ok(default),
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err("invalid boolean".into()),
    }
}

#[cfg(test)]
#[path = "connection_request_config_tests.rs"]
mod connection_request_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::words;
    #[test]
    fn auth_versions_and_atomic_overrides() {
        use crate::omci_auth::Policy;
        assert!(matches!(
            parse_expanded("").unwrap().omci_auth.global,
            Policy::Disabled
        ));
        let global = "config omci_auth 'omci_auth_global'\n option schema_version '1'\n option auth_mode 'loid_password'\n option auth_loid 'test-onu'\n option auth_password 'test-pass'\n";
        let device = "config device 'child'\n option optical_sn 'TEST12345678'\n option cwmp_id 'child-id'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n";
        let inherited = parse_expanded(&format!("{global}{device}")).unwrap();
        assert!(inherited.omci_auth.devices.is_empty());
        assert!(inherited.omci_auth.effective("TEST12345678") == &inherited.omci_auth.global);
        assert!(parse_expanded(&format!(
            "{global}{device} option auth_mode 'loid_password'\n option auth_loid 'local'\n"
        ))
        .is_err());
        let disabled =
            parse_expanded(&format!("{global}{device} option auth_mode 'disabled'\n")).unwrap();
        assert!(matches!(
            disabled.omci_auth.effective("TEST12345678"),
            Policy::Disabled
        ));
        let changed = parse_expanded(&format!("{global}{device} option auth_mode 'loid_password'\n option auth_loid 'local'\n option auth_password 'local-pass'\n")).unwrap();
        assert_ne!(inherited.revision, changed.revision);
        let before = parse(global).unwrap();
        let after = parse(&global.replace("test-pass", "new-pass")).unwrap();
        assert_eq!(before.business_revision(), after.business_revision());
        assert!(parse_expanded(&global.replace("'1'", "'2'")).is_err());
        assert!(parse_expanded(&global.replace("'1'", "'0'")).is_err());
        assert!(parse_expanded(&global.replace("'loid_password'", "'legacy_v0'")).is_err());
        assert!(parse_expanded(&format!("{global} option auth_mode 'disabled'\n")).is_err());
        assert!(parse_expanded(&global.replace("option auth_loid", "list auth_loid")).is_err());
    }
    #[test]
    fn service_list_tokens_are_normalized_and_tr069_is_reserved() {
        assert_eq!(
            normalize_service_list(&[" iptv,VOIP ".into(), "IPTV".into()]).unwrap(),
            ["IPTV", "VOIP"]
        );
        assert!(normalize_service_list(&["INTERNET, TR069".into()])
            .unwrap_err()
            .contains("TR069"));
        assert!(normalize_custom_vlan_service_list(&["UNKNOWN".into()]).is_err());
    }
    #[test]
    fn quoted_export() {
        assert_eq!(
            words("option ssid 'a'\\''b c'").unwrap(),
            ["option", "ssid", "a'b c"]
        );
    }
    #[test]
    fn rejects_invalid_policy() {
        assert!(parse("config provisioning 'global'\n option mode 'invalid'").is_err());
    }
    #[test]
    fn accepts_program_start_trigger_and_rejects_unknown_triggers() {
        let config = parse(
            "config provisioning 'global'\n option mode 'auto'\n list trigger 'program_start'",
        )
        .unwrap();
        assert!(config.triggers.contains("program_start"));
        assert!(parse("config provisioning 'global'\n list trigger 'unknown_trigger'").is_err());
    }
    #[test]
    fn parses_periodic_interval_minutes_and_rejects_old_interval_option() {
        let config = parse("config provisioning 'global'\n option interval_minutes '30'").unwrap();
        assert_eq!(config.interval_seconds, 1800);
        assert!(parse("config provisioning 'global'\n option interval '3600'").is_err());
    }
    #[test]
    fn listener_changes_reload_configuration_without_changing_business_revision() {
        let first = parse("config acs 'acs'\n option password 'one'").unwrap();
        let second = parse("config acs 'acs'\n option password 'two'").unwrap();
        assert_ne!(first.revision, second.revision);
        assert_eq!(first.business_revision(), second.business_revision());
    }
    #[test]
    fn hwtc_selection_is_explicit_and_legacy_alias_is_rejected() {
        let config = |adapter: &str| {
            format!("config device 'child'\n option optical_sn 'HWTC1234ABCD'\n option cwmp_id 'child-id'\n option omci_adapter '{adapter}'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'")
        };
        assert_eq!(
            parse_expanded(&config("hwtc")).is_ok(),
            cfg!(feature = "device-hwtc")
        );
        assert_eq!(parse_expanded(&config("k155")).is_ok(), false);
    }
    #[test]
    fn provisioning_has_no_optical_policy() {
        assert!(parse_expanded("").is_ok());
    }

    #[test]
    fn parses_allow_unsupported_as_bridge_and_generic_adapter() {
        assert!(
            parse_expanded("config provisioning 'global'\n")
                .unwrap()
                .allow_unsupported_as_bridge
        );
        assert!(parse_expanded("").unwrap().allow_unsupported_as_bridge);

        let text = "config provisioning 'global'\n option allow_unsupported_as_bridge '1'\nconfig device 'child'\n option optical_sn 'ZTEG12345678'\n option cwmp_id 'child-id'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n";
        let config = parse_expanded(text).unwrap();
        assert!(config.allow_unsupported_as_bridge);
        assert_eq!(config.devices[0].omci_adapter, "generic");

        let text_disabled =
            "config provisioning 'global'\n option allow_unsupported_as_bridge '0'\n";
        let config_disabled = parse_expanded(text_disabled).unwrap();
        assert!(!config_disabled.allow_unsupported_as_bridge);
    }
}
