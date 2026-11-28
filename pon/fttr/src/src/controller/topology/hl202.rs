//! Whitelisted, typed projection of already cached, identity-bound HL202 reports.
use super::*;

pub(super) fn collect(
    engine: &std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: &crate::controller::service::States,
) -> BTreeMap<String, TopologyHl202> {
    let mut result = BTreeMap::new();
    let list = crate::controller::hl202::control_query(HlQuery::List, engine, services);
    let Ok(HlListReply::List { devices, .. }) = serde_json::from_value(list) else {
        return result;
    };
    for device in devices.into_iter().filter(|d| d.optical_online) {
        let Some(source) = device.mqtt.filter(|s| s.reason == "ready") else {
            continue;
        };
        let query = HlQuery::Report {
            target: HlOpticalSelection {
                optical_sn: device.optical_sn.clone(),
                onu_id: device.onu_id,
                binding_generation: source.binding_generation.clone(),
            },
        };
        let reply = crate::controller::hl202::control_query(query, engine, services);
        let Ok(HlReportReply::Report {
            target,
            report: Some(report),
            ..
        }) = serde_json::from_value(reply)
        else {
            continue;
        };
        if target.binding_generation != source.binding_generation
            || target.optical_sn.as_deref() != Some(&device.optical_sn)
            || target.onu_id != Some(device.onu_id)
            || target.reason != "ready"
        {
            continue;
        }
        if let Some(extension) = project(report, target.binding_generation, unix_time()) {
            if let Some(sn) = serial(&device.optical_sn) {
                result.insert(sn, extension);
            }
        }
    }
    result
}

pub(super) fn project(state: HlReportState, generation: String, now: u64) -> Option<TopologyHl202> {
    let good = state.last_good?;
    // Do not attach historical telemetry to a currently live optical node.
    if good > now || now - good >= 90 || state.error.is_some() {
        return None;
    }
    let report = state.report?;
    let string = |key: &str| {
        report
            .fields
            .get(key)
            .and_then(|f| f.value.as_str())
            .and_then(nonempty)
    };
    let number = |key: &str| report.fields.get(key).and_then(|f| f.value.as_u64());
    Some(TopologyHl202 {
        report_revision: state.revision,
        binding_generation: generation,
        age_seconds: now - good,
        ip: string("ip")
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string()),
        bridge_ip: string("bridge_ip")
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string()),
        mac: string("mac").as_deref().and_then(normalize_mac),
        connection_type_raw: string("connection_type"),
        uptime_seconds: report
            .fields
            .get("uptime_seconds")
            .and_then(|f| f.value.as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0),
        cpu_usage_raw: number("cpu_usage"),
        memory_usage_raw: number("memory_usage"),
    })
}
