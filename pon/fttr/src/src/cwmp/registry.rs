//! Explicit contract identifiers; an unavailable profile never silently falls back.
use super::{contract::Contract, vendor};
pub const VERSION: u32 = 5;
pub fn available() -> Vec<&'static Contract> {
    let specialized: &[&Contract] = &[
        #[cfg(feature = "device-hl202")]
        &crate::devices::hl202::cwmp::CONTRACT,
        #[cfg(feature = "device-hwtc")]
        &crate::devices::hwtc::cwmp::CONTRACT,
        #[cfg(feature = "device-hwtc")]
        &crate::devices::hwtc::cwmp::V152_CONTRACT,
        #[cfg(feature = "device-hwtc")]
        &crate::devices::hwtc::cwmp::K155_CONTRACT,
        #[cfg(feature = "device-zte")]
        &crate::devices::zte::g1612::cwmp::CONTRACT,
    ];
    vendor::candidates()
        .into_iter()
        .chain(specialized.iter().copied())
        .collect()
}
pub fn lookup(id: &str) -> Result<&'static Contract, String> {
    available()
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("CWMP contract unavailable: {id}"))
}
