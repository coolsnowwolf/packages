//! Independent, opt-in (default-off at runtime, default-on at compile time)
//! bridge-mode MQTT redirect coordinator.
//!
//! Never starts or alters the embedded MQTT broker, ACS, OMCI, or NAT-mode
//! devices whose addresses come from the master gateway's LAN DHCP pool.
#[cfg(feature = "mqtt-bridge")]
mod capture;
#[cfg(feature = "mqtt-bridge")]
mod evidence;
#[cfg(feature = "mqtt-bridge")]
mod lan;
#[cfg(feature = "mqtt-bridge")]
mod nft;
#[cfg(feature = "mqtt-bridge")]
mod reconcile;

#[cfg(feature = "mqtt-bridge")]
pub(crate) use reconcile::{
    configure, device_view, global_view, set_broker_address, shutdown, tick,
};

#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn configure(_enabled: bool) {}
#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn set_broker_address(_addr: Option<std::net::Ipv4Addr>) {}
#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn shutdown() {}
#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn tick(
    _online_onus: &std::collections::BTreeMap<u16, String>,
    _interfaces: &std::collections::BTreeMap<u16, String>,
    _configured_devices: &[crate::provisioning::model::Device],
) {
}
#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn global_view() -> serde_json::Value {
    serde_json::Value::Null
}
#[cfg(not(feature = "mqtt-bridge"))]
pub(crate) fn device_view(_optical_sn: &str) -> serde_json::Value {
    serde_json::Value::Null
}
