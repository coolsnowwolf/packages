//! Arbitrary-IP ACS/CWMP hijack and redirect coordinator for FTTR ONUs.
//!
//! Default-off at runtime (`uci set fttr.acs_bridge.enabled=0`), default-on at
//! compile time (`acs-bridge` Cargo feature). Targets only active FTTR ONU
//! interfaces (`fttrN` on `br-lan`) and the isolated `br-tr069` management VLAN,
//! never ordinary LAN `eth*` or `wlan*` clients.
#[cfg(feature = "acs-bridge")]
mod nft;
#[cfg(feature = "acs-bridge")]
mod reconcile;

#[cfg(feature = "acs-bridge")]
pub(crate) use reconcile::{configure, global_view, shutdown, tick};

#[cfg(not(feature = "acs-bridge"))]
pub(crate) fn configure(_config: &crate::provisioning::model::AcsBridgeConfig) {}

#[cfg(not(feature = "acs-bridge"))]
pub(crate) fn shutdown() {}

#[cfg(not(feature = "acs-bridge"))]
pub(crate) fn tick(
    _online_onus: &std::collections::BTreeMap<u16, String>,
    _interfaces: &std::collections::BTreeMap<u16, String>,
    _configured_devices: &[crate::provisioning::model::Device],
    _management_network: Option<&crate::provisioning::management_network::Desired>,
    _management_active: bool,
    _acs_devices: &crate::cwmp::observations::DeviceObservations,
    _source_links: Option<&std::collections::BTreeMap<String, String>>,
) {
}

#[cfg(not(feature = "acs-bridge"))]
pub(crate) fn global_view() -> serde_json::Value {
    serde_json::Value::Null
}
