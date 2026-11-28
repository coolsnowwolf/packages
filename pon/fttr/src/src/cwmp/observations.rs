//! Small ACS projection consumed by the scheduler. Never contains RPC results,
//! connection request URLs, Inform parameters, or presentation counters.
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct DeviceObservation {
    pub serial: String,
    pub product_class: String,
    pub peer: Option<std::net::Ipv4Addr>,
    pub software_version: String,
    pub hardware_version: String,
    pub boot: bool,
    pub endpoint_id: String,
    pub last_inform: u64,
}

pub(crate) type DeviceObservations = BTreeMap<String, DeviceObservation>;

#[derive(Default)]
pub(crate) struct ListenerObservation {
    pub enabled: bool,
    pub listen: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
    pub primary: String,
    pub extra: Option<String>,
}

impl ListenerObservation {
    pub(crate) fn current_inform(&self, device: &DeviceObservation) -> Option<u64> {
        (self.enabled
            && self.error.is_none()
            && !device.endpoint_id.is_empty()
            && (device.endpoint_id == self.primary
                || self.extra.as_deref() == Some(&device.endpoint_id)))
        .then_some(device.last_inform)
    }
}

impl DeviceObservation {
    pub(crate) fn inform_metadata(inform: &serde_json::Value) -> (String, String, bool) {
        let mut software = None;
        let mut hardware = None;
        for parameter in inform["parameters"].as_array().into_iter().flatten() {
            let name = parameter["name"].as_str().unwrap_or_default();
            if software.is_none() && name.ends_with(".SoftwareVersion") {
                software = Some(parameter["value"].as_str().unwrap_or_default());
            }
            if hardware.is_none() && name.ends_with(".HardwareVersion") {
                hardware = Some(parameter["value"].as_str().unwrap_or_default());
            }
            if software.is_some() && hardware.is_some() {
                break;
            }
        }
        let boot = inform["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|e| matches!(e.as_str(), Some("0 BOOTSTRAP" | "1 BOOT")))
        });
        (
            software.unwrap_or_default().into(),
            hardware.unwrap_or_default().into(),
            boot,
        )
    }
}

#[cfg(test)]
pub(crate) fn from_fixture(input: &serde_json::Value) -> DeviceObservations {
    input["devices"]
        .as_object()
        .into_iter()
        .flat_map(|entries| entries.iter())
        .map(|(id, row)| {
            let string = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
            let (software_version, hardware_version, boot) =
                DeviceObservation::inform_metadata(&row["inform"]);
            (
                id.clone(),
                DeviceObservation {
                    serial: string("serial"),
                    product_class: string("product_class"),
                    peer: row["peer"].as_str().and_then(|value| value.parse().ok()),
                    software_version,
                    hardware_version,
                    boot,
                    endpoint_id: string("endpoint_id"),
                    last_inform: row["last_inform"].as_u64().unwrap_or_default(),
                },
            )
        })
        .collect()
}
