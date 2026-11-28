use super::{Field, Member, Report};
use std::{collections::BTreeMap, net::IpAddr};

enum Format {
    Text,
    Address,
    Mac,
    Decimal,
    Integer,
}

fn contract(name: &str) -> Option<(&'static str, Format, Option<&'static str>)> {
    Some(match name {
        "DeviceInfo_devParaStatic.ModelName" => ("model", Format::Text, None),
        "DeviceInfo_devParaStatic.Manufacturer" => ("manufacturer", Format::Text, None),
        "DeviceInfo_devParaDynamic.ManufacturerOUI" => ("manufacturer_oui", Format::Text, None),
        "DeviceInfo_devParaDynamic.SerialNum" => ("serial", Format::Text, None),
        "IP" => ("ip", Format::Address, None),
        "br0ip" => ("bridge_ip", Format::Address, None),
        "conn_type" => ("connection_type", Format::Text, None),
        "al-mac" => ("mac", Format::Mac, None),
        "Sys_Entry.uptime" => ("uptime_seconds", Format::Decimal, Some("seconds")),
        "Info_SysDevice.CpuUsage" => ("cpu_usage", Format::Integer, None),
        "Info_SysDevice.MemoryUsage" => ("memory_usage", Format::Integer, None),
        _ => return None,
    })
}

fn value(text: &str, format: Format) -> Option<serde_json::Value> {
    let text = text.trim();
    match format {
        Format::Text => (!text.is_empty() && !text.contains('\0')).then(|| serde_json::json!(text)),
        Format::Address => text
            .parse::<IpAddr>()
            .ok()
            .map(|address| serde_json::json!(address.to_string())),
        Format::Mac => {
            crate::mqtt_identity::normalize_target(text).map(|mac| serde_json::json!(mac))
        }
        Format::Decimal => text
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| serde_json::json!(value)),
        Format::Integer => text
            .parse::<u64>()
            .ok()
            .map(|value| serde_json::json!(value)),
    }
}

pub(super) fn report(
    members: Vec<Member>,
    compressed_bytes: usize,
    decompressed_bytes: usize,
) -> Report {
    let mut fields = BTreeMap::new();
    let mut errors = Vec::new();
    for member in &members {
        if let Some((key, format, unit)) = contract(&member.name) {
            match member.text.as_deref().and_then(|text| value(text, format)) {
                Some(value) => {
                    fields.insert(
                        key.into(),
                        Field {
                            value,
                            member: member.name.clone(),
                            unit: unit.map(str::to_owned),
                        },
                    );
                }
                None => errors.push(format!("unsupported value in {}", member.name)),
            }
        }
    }
    Report {
        schema_version: 1,
        compressed_bytes,
        decompressed_bytes,
        fields,
        members,
        errors,
    }
}
