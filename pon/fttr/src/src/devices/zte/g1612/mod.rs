//! ZXHN G1612 identity rules and configuration.
pub mod cwmp;
pub mod service;

pub fn matches(_vendor: &str, equipment: Option<&str>, serial: Option<&str>) -> bool {
    if equipment == Some("ZXHN G1612") || equipment.is_some_and(|eq| eq.contains("G1612")) {
        return true;
    }
    if let Some(sn) = serial {
        if sn.starts_with("9866") && equipment.is_none() {
            return true;
        }
    }
    false
}

pub fn matches_cwmp(optical_sn: &str, product_class: &str, serial: &str) -> bool {
    if !product_class.contains("G1612") && product_class != "ZXHN G1612" {
        return false;
    }
    if optical_sn.len() >= 8 && serial.contains(optical_sn) {
        return true;
    }
    if optical_sn.starts_with("9866") && optical_sn.len() == 12 {
        return serial.contains(optical_sn) || serial.contains(&optical_sn[4..]);
    }
    serial.contains(optical_sn)
}

pub fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    service::configure(platform, id, cancellation)
}
