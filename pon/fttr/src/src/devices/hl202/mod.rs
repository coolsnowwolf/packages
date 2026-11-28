//! HL202-DU-C: verified OMCI topology, private authentication and MQTT shell contract.
pub mod api;
pub mod command;
#[cfg(feature = "mqtt")]
pub mod command_binding;
#[cfg(feature = "mqtt")]
mod command_transport;
pub mod cwmp;
#[cfg(feature = "mqtt")]
pub mod management_wan;
#[cfg(feature = "mqtt")]
pub mod mqtt_lifecycle;
#[cfg(feature = "mqtt")]
pub mod provisioning;
pub mod request;
pub mod script;
pub mod service;
pub mod telemetry;
pub(super) fn cwmp_credentials(product_class: &str) -> Option<super::CwmpCredentials> {
    (product_class == "HL202-DU-C").then(|| super::CwmpCredentials {
        username: "rms".into(),
        password: "rms".into(),
    })
}
pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "H3CT" && equipment == Some("HL202-DU-C")
}
#[cfg(feature = "mqtt")]
pub mod accounts;
pub(crate) const AUTH_LAYOUT: crate::omci_auth::Layout = crate::omci_auth::Layout {
    name: "hl202-private",
    class: 65530,
    loid_mask: 0x4000,
    password_mask: 0x2000,
    status_mask: 0x1000,
    loid_width: 10,
    password_width: 10,
    recover_alloc_channel: true,
};
