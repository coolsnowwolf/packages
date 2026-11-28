//! Convert business intent to a parameter plan using resolved object instances.
use super::{codec::encode_services, contract::Contract, path::*};
use crate::provisioning::model::BindingIntent;
use crate::provisioning::model::{ConnectionMode, Parameter, ParameterKind, VlanMode, WanIntent};

pub struct Plan {
    pub parameters: Vec<Parameter>,
    pub guard: String,
}

pub fn compile_wlan(
    intent: &crate::provisioning::model::WlanIntent,
    contract: &Contract,
    endpoints: &EndpointMap,
) -> Result<Vec<Parameter>, String> {
    if contract.capability_groups.wlan != super::contract::Support::Supported {
        return Err("WLAN contract is unverified".into());
    }
    if intent.ssid.is_empty() || intent.ssid.len() > 32 {
        return Err("SSID must be 1..32 bytes".into());
    }
    let root = format!("{}.", endpoints.resolve(&intent.endpoint)?);
    let mut output = Vec::new();
    let mut add = |name: &str, value: String, kind, secret| {
        output.push(Parameter {
            name: format!("{root}{name}"),
            value,
            kind,
            secret,
        })
    };
    add("SSID", intent.ssid.clone(), ParameterKind::String, false);
    if let Some(value) = intent.enabled {
        add(
            "Enable",
            if value { "1" } else { "0" }.into(),
            ParameterKind::Boolean,
            false,
        );
    }
    if let Some(value) = intent.broadcast {
        add(
            "BeaconAdvertisementEnabled",
            if value { "1" } else { "0" }.into(),
            ParameterKind::Boolean,
            false,
        );
    }
    if intent.enabled.unwrap_or(true) {
        add("RadioEnabled", "1".into(), ParameterKind::Boolean, false);
    }
    let security = intent.security.or_else(|| {
        intent
            .key
            .as_ref()
            .map(|_| crate::provisioning::model::WifiSecurity::Wpa2Psk)
    });
    if let Some(sec) = security {
        use crate::provisioning::model::WifiSecurity;
        match sec {
            WifiSecurity::None => {
                add("BeaconType", "None".into(), ParameterKind::String, false);
                add(
                    "BasicEncryptionModes",
                    "None".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "BasicAuthenticationMode",
                    "None".into(),
                    ParameterKind::String,
                    false,
                );
            }
            WifiSecurity::Wpa2Psk => {
                let key = intent.key.as_deref().ok_or("WPA2 key required")?;
                if !(8..=63).contains(&key.len()) {
                    return Err("WPA2 key must be 8..63 bytes".into());
                }
                add(
                    "BeaconType",
                    contract.wlan.wpa2_beacon.into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iEncryptionModes",
                    "AESEncryption".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iAuthenticationMode",
                    "PSKAuthentication".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "PreSharedKey.1.KeyPassphrase",
                    key.into(),
                    ParameterKind::String,
                    true,
                );
            }
            WifiSecurity::Wpa3Sae => {
                let key = intent.key.as_deref().ok_or("WPA3 key required")?;
                if !(8..=63).contains(&key.len()) {
                    return Err("WPA3 key must be 8..63 bytes".into());
                }
                add("BeaconType", "WPA3".into(), ParameterKind::String, false);
                add(
                    "IEEE11iEncryptionModes",
                    "AESEncryption".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iAuthenticationMode",
                    "PSKAuthentication".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "PreSharedKey.1.KeyPassphrase",
                    key.into(),
                    ParameterKind::String,
                    true,
                );
            }
            WifiSecurity::Wpa2PskWpa3Sae => {
                let key = intent.key.as_deref().ok_or("WPA2/WPA3 key required")?;
                if !(8..=63).contains(&key.len()) {
                    return Err("WPA2/WPA3 key must be 8..63 bytes".into());
                }
                add(
                    "BeaconType",
                    "WPA2/WPA3".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iEncryptionModes",
                    "AESEncryption".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iAuthenticationMode",
                    "PSKAuthentication".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "PreSharedKey.1.KeyPassphrase",
                    key.into(),
                    ParameterKind::String,
                    true,
                );
            }
            WifiSecurity::WpaPskWpa2Psk => {
                let key = intent.key.as_deref().ok_or("WPA/WPA2 key required")?;
                if !(8..=63).contains(&key.len()) {
                    return Err("WPA/WPA2 key must be 8..63 bytes".into());
                }
                add(
                    "BeaconType",
                    contract.wlan.wpa_wpa2_beacon.into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "WPAEncryptionModes",
                    "TKIPandAESEncryption".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "WPAAuthenticationMode",
                    "PSKAuthentication".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iEncryptionModes",
                    "TKIPandAESEncryption".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "IEEE11iAuthenticationMode",
                    "PSKAuthentication".into(),
                    ParameterKind::String,
                    false,
                );
                add(
                    "PreSharedKey.1.KeyPassphrase",
                    key.into(),
                    ParameterKind::String,
                    true,
                );
            }
        }
    }
    Ok(output)
}

pub fn compile_binding(
    intent: &crate::provisioning::model::BindingIntent,
    contract: &Contract,
    objects: &WanObjects,
    endpoints: &EndpointMap,
) -> Result<Plan, String> {
    if intent.endpoints.is_empty() {
        return Err("binding requires logical endpoints".into());
    }
    if contract.capability_groups.binding == super::contract::Support::Unsupported {
        return Ok(Plan {
            parameters: Vec::new(),
            guard: String::new(),
        });
    }
    let _field = contract
        .wan
        .binding
        .ok_or("LAN/SSID binding contract is unverified")?;
    if contract.capability_groups.binding != super::contract::Support::Supported
        || contract.references.is_none()
    {
        return Err("binding capability/reference encoding is unverified".into());
    }
    let (parent, connection) = objects.paths()?;
    if let Some(super::contract::ReferenceEncoding::EndpointFlags { lan, wlan }) =
        contract.references
    {
        let mut selected = std::collections::BTreeSet::new();
        for endpoint in &intent.endpoints {
            let path = endpoints.resolve(endpoint)?;
            let (kind, index) = path.rsplit_once('.').ok_or("invalid binding endpoint")?;
            let index: usize = index.parse().map_err(|_| "invalid binding instance")?;
            let offset = if kind.ends_with(".LANEthernetInterfaceConfig")
                && (1..=lan.len()).contains(&index)
            {
                index - 1
            } else if kind.ends_with(".WLANConfiguration") && (1..=wlan.len()).contains(&index) {
                lan.len() + index - 1
            } else {
                return Err("endpoint outside contract binding switch range".into());
            };
            selected.insert(offset);
        }
        return Ok(Plan {
            guard: contract.wan.service.path(&parent, &connection),
            parameters: contract
                .binding_fields()
                .into_iter()
                .enumerate()
                .map(|(index, field)| Parameter {
                    name: field.path(&parent, &connection),
                    value: if selected.contains(&index) { "1" } else { "0" }.into(),
                    kind: ParameterKind::UnsignedInt,
                    secret: false,
                })
                .collect(),
        });
    }
    let field = _field;
    let values: Result<Vec<_>, _> = intent
        .endpoints
        .iter()
        .map(|e| endpoints.resolve(e))
        .collect();
    Ok(Plan {
        guard: contract.wan.service.path(&parent, &connection),
        parameters: vec![Parameter {
            name: field.path(&parent, &connection),
            value: values?.join(","),
            kind: ParameterKind::String,
            secret: false,
        }],
    })
}

/// Compile only after contract resolution. WAN ownership later recompiles plans
/// against the actual AddObject/inventory instance, retaining disabled Enable.
pub fn compile_device(device: &mut crate::provisioning::model::Device, endpoints: &EndpointMap) {
    let contract = device.contract();
    for item in &mut device.items {
        let objects = WanObjects {
            parent: item.auto_instance.unwrap_or(1),
            connection: 1,
            kind: ConnectionKind::Ip,
        };
        let result = if let Some(intent) = &item.wan_intent {
            compile_wan(intent, contract, &objects, endpoints)
                .map(|plan| (plan.parameters, Some(plan.guard)))
        } else if let Some(intent) = &item.wlan_intent {
            compile_wlan(intent, contract, endpoints).map(|p| (p, None))
        } else if let Some(intent) = &item.binding_intent {
            compile_binding(intent, contract, &objects, endpoints)
                .map(|plan| (plan.parameters, Some(plan.guard)))
        } else {
            continue;
        };
        match result {
            Ok((parameters, guard)) => {
                item.parameters = parameters;
                item.guard = guard;
                item.blocked = None;
            }
            Err(reason) => {
                item.parameters.clear();
                item.blocked = Some(reason);
            }
        }
    }
}

pub fn compile_wan(
    intent: &WanIntent,
    contract: &Contract,
    objects: &WanObjects,
    endpoints: &EndpointMap,
) -> Result<Plan, String> {
    if contract.capability_groups.wan != super::contract::Support::Supported {
        return Err("CWMP write contract is unverified".into());
    }
    if intent.services.iter().any(|s| {
        !contract
            .supported_services
            .iter()
            .any(|known| s.eq_ignore_ascii_case(known))
    }) {
        return Err("requested service encoding is unverified by this contract".into());
    }
    if intent.mode != ConnectionMode::Bridge || matches!(objects.kind, ConnectionKind::Ppp) {
        return Err("business WAN mode is not verified by this contract".into());
    }
    let vlan = match intent.vlan {
        VlanMode::Untagged => 0,
        VlanMode::Tagged(v) if (1..=4094).contains(&v) => v,
        _ => return Err("invalid VLAN".into()),
    };
    if intent.priority > 7 {
        return Err("invalid priority".into());
    }
    if intent
        .services
        .iter()
        .any(|s| super::codec::service_tokens(s).any(|t| t.eq_ignore_ascii_case("TR069")))
    {
        return Err("management service cannot be compiled as a business WAN".into());
    }
    if vlan == 0
        && (intent.internet
            || intent
                .services
                .iter()
                .any(|s| s.eq_ignore_ascii_case("INTERNET")))
    {
        return Err("untagged INTERNET is reserved for fallback".into());
    }
    let (parent, connection) = objects.paths()?;
    let w = contract.wan;
    let mut parameters = Vec::new();
    let mut add = |name: String, value: String, kind| {
        parameters.push(Parameter {
            name,
            value,
            kind,
            secret: false,
        })
    };
    add(
        format!("{connection}ConnectionType"),
        "IP_Bridged".into(),
        ParameterKind::String,
    );
    add(
        format!("{connection}NATEnabled"),
        "0".into(),
        ParameterKind::Boolean,
    );
    if !intent.services.is_empty() {
        let tokens: Vec<_> = intent.services.iter().map(String::as_str).collect();
        let value = encode_services(&tokens, contract.service_encoding).map_err(str::to_owned)?;
        add(
            w.service.path(&parent, &connection),
            value,
            ParameterKind::String,
        );
    }
    if intent.internet {
        if let Some(f) = w.cloud_cpe {
            add(
                f.path(&parent, &connection),
                "1".into(),
                ParameterKind::Boolean,
            );
        }
    }
    if let Some(f) = w.enabled {
        add(
            f.path(&parent, &connection),
            if vlan == 0 { "0" } else { "1" }.into(),
            ParameterKind::Boolean,
        );
    }
    add(
        w.vlan.path(&parent, &connection),
        vlan.to_string(),
        ParameterKind::UnsignedInt,
    );
    add(
        w.priority.path(&parent, &connection),
        intent.priority.to_string(),
        ParameterKind::UnsignedInt,
    );
    if !intent.bindings.is_empty() {
        let plan = compile_binding(
            &BindingIntent {
                endpoints: intent.bindings.clone(),
            },
            contract,
            objects,
            endpoints,
        )?;
        for p in plan.parameters {
            add(p.name, p.value, p.kind);
        }
    }
    add(
        format!("{connection}Enable"),
        if intent.enabled { "1" } else { "0" }.into(),
        ParameterKind::Boolean,
    );
    Ok(Plan {
        parameters,
        guard: w.service.path(&parent, &connection),
    })
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "device-hl202", feature = "device-hwtc"))]
    use super::*;
    #[cfg(feature = "device-hwtc")]
    #[test]
    fn v152_binding_switches_use_actual_instances_and_clear_unselected_ports() {
        use crate::provisioning::model::EndpointRef;
        let c = super::super::registry::lookup("hwtc-v152-hw").unwrap();
        let objects = WanObjects {
            parent: 3,
            connection: 2,
            kind: ConnectionKind::Ip,
        };
        let binding = BindingIntent {
            endpoints: vec![EndpointRef::Lan(1), EndpointRef::Ssid { radio: 5, slot: 2 }],
        };
        let mut map = EndpointMap::default();
        assert!(compile_binding(&binding, c, &objects, &map).is_err());
        map.0.insert(
            EndpointRef::Lan(1),
            "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.1".into(),
        );
        map.0.insert(
            EndpointRef::Ssid { radio: 5, slot: 2 },
            "InternetGatewayDevice.LANDevice.1.WLANConfiguration.6".into(),
        );
        let plan = compile_binding(&binding, c, &objects, &map).unwrap();
        assert_eq!(plan.parameters.len(), 12);
        assert_eq!(plan.parameters.iter().filter(|p| p.value == "1").count(), 2);
        for p in &plan.parameters {
            assert!(p
                .name
                .contains("WANConnectionDevice.3.WANIPConnection.2.X_HW_LANBIND."));
            assert_eq!(p.kind, ParameterKind::UnsignedInt);
            assert_eq!(
                p.value,
                if p.name.ends_with(".Lan1Enable") || p.name.ends_with(".SSID6Enable") {
                    "1"
                } else {
                    "0"
                }
            );
        }
        let mut wan = intent();
        wan.bindings = binding.endpoints;
        let wan_plan = compile_wan(&wan, c, &objects, &map).unwrap();
        assert_eq!(
            wan_plan
                .parameters
                .iter()
                .filter(|p| p.name.contains(".X_HW_LANBIND."))
                .count(),
            12
        );
        map.0.insert(
            EndpointRef::Lan(1),
            "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.5".into(),
        );
        assert!(compile_wan(&wan, c, &objects, &map).is_err());
    }
    #[cfg(any(feature = "device-hl202", feature = "device-hwtc"))]
    fn intent() -> WanIntent {
        WanIntent {
            services: vec!["OTHER".into()],
            vlan: VlanMode::Tagged(66),
            priority: 6,
            mode: ConnectionMode::Bridge,
            bindings: vec![],
            internet: false,
            enabled: true,
        }
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn intent_resolves_actual_instances_and_rejects_management() {
        let c = super::super::registry::lookup("hwtc-hw").unwrap();
        #[cfg(feature = "device-hwtc")]
        {
            let objects = WanObjects {
                parent: 7,
                connection: 3,
                kind: ConnectionKind::Ip,
            };
            let plan = compile_wan(&intent(), c, &objects, &EndpointMap::default()).unwrap();
            assert!(plan
                .parameters
                .iter()
                .all(|p| p.name.contains("WANConnectionDevice.7.WANIPConnection.3.")));
            let mut i = intent();
            i.services = vec!["TR069_INTERNET".into()];
            assert!(compile_wan(&i, c, &objects, &EndpointMap::default()).is_err());
            i = intent();
            i.bindings
                .push(crate::provisioning::model::EndpointRef::Lan(2));
            assert!(compile_wan(&i, c, &objects, &EndpointMap::default()).is_err());
        }
        let _ = c;
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn logical_binding_requires_inventory_and_parent_vlan_stays_on_parent() {
        use crate::provisioning::model::EndpointRef;
        let mut i = intent();
        i.bindings.push(EndpointRef::Lan(2));
        let c = super::super::registry::lookup("hl202-cu").unwrap();
        let objects = WanObjects {
            parent: 5,
            connection: 4,
            kind: ConnectionKind::Ip,
        };
        assert!(compile_wan(&i, c, &objects, &EndpointMap::default()).is_err());
        let mut map = EndpointMap::default();
        map.0.insert(
            EndpointRef::Lan(2),
            "InternetGatewayDevice.LANDevice.1.LANEthernetInterfaceConfig.7".into(),
        );
        let plan = compile_wan(&i, c, &objects, &map).unwrap();
        assert!(plan
            .parameters
            .iter()
            .any(|p| p.name.ends_with("WANConnectionDevice.5.X_CU_VLAN") && p.value == "66"));
        assert!(plan
            .parameters
            .iter()
            .any(|p| p.name.ends_with("WANIPConnection.4.X_CU_LanInterface")
                && p.value.ends_with(".7")));
    }
    #[cfg(feature = "device-hwtc")]
    #[test]
    fn k155_binding_unsupported_produces_empty_plan() {
        use crate::provisioning::model::EndpointRef;
        let c = super::super::registry::lookup("hwtc-k155-hw").unwrap();
        let endpoints = EndpointMap::default();
        let binding = BindingIntent {
            endpoints: vec![EndpointRef::Lan(1)],
        };
        let objects = WanObjects {
            parent: 1,
            connection: 1,
            kind: ConnectionKind::Ip,
        };
        let plan = compile_binding(&binding, c, &objects, &endpoints).unwrap();
        assert!(plan.parameters.is_empty());
    }
}
