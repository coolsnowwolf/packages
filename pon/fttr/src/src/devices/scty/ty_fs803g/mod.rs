//! COMHEART TY-FS803G, observed with MGS.V1.0.0.0 / HV1.0.0.0.
//!
//! Direct OMCI bridge profile with Broadcom BCM68252 RDPA Root TM activation.
//! This is an OMCI bridge profile, not a CWMP identity or Wi-Fi contract.

pub mod service;

pub(super) fn matches(equipment: Option<&str>) -> bool {
    equipment == Some("COMHEART TY-FS803G")
}

pub(super) fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    service::configure(platform, id, cancellation)
}
