//! Fresh MQTT source identities, independent of ACS configuration and Inform readiness.
mod registry;
pub use registry::Registry;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const FRESHNESS: u64 = 60;

#[derive(Clone, Default)]
pub(crate) struct Transport {
    pub last_seen: u64,
    pub epoch: u64,
}

pub use fttr_api::HlOpticalSelection as Selection;

pub use fttr_api::MqttSource as Snapshot;

pub(crate) fn normalize_target(topic: &str) -> Option<String> {
    let value = topic.strip_suffix("/fttr_data.tar.gz").unwrap_or(topic);
    let valid = match value.len() {
        12 => value.bytes().all(|b| b.is_ascii_hexdigit()),
        17 => value.bytes().enumerate().all(|(i, b)| {
            if i % 3 == 2 {
                b == b':'
            } else {
                b.is_ascii_hexdigit()
            }
        }),
        _ => false,
    };
    valid.then(|| value.replace(':', "").to_ascii_uppercase())
}

fn automatic_target<'a>(
    transport: &'a BTreeMap<String, Transport>,
    links: &BTreeMap<String, String>,
    selection: (&str, u64),
) -> Result<Option<(&'a str, &'a Transport)>, &'static str> {
    let (serial, time) = selection;
    let mut matches = transport.iter().filter(|(mac, observation)| {
        observation.last_seen <= time
            && time - observation.last_seen <= FRESHNESS
            && links.get(*mac).is_some_and(|sn| sn == serial)
    });
    match (matches.next(), matches.next()) {
        (None, _) => Ok(None),
        (Some((mac, source)), None) => Ok(Some((mac.as_str(), source))),
        _ => Err(
            "multiple fresh MQTT targets originate from this ONU; automatic identity is ambiguous",
        ),
    }
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
pub(crate) fn shared() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Mutex::new(Registry::new(format!("{}-{epoch:x}", std::process::id())))
    })
}

#[cfg(test)]
#[path = "mqtt_identity/tests.rs"]
mod tests;
