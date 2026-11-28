//! HWTC standard OMCI bridge service; proprietary shell management unavailable.
mod bridge;
pub mod cwmp;
pub mod k155;
pub mod service;
pub mod v152;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Standard,
    K155,
    V152,
}

pub(super) fn profile(equipment: Option<&str>) -> Profile {
    match equipment {
        Some("K155-11") => Profile::K155,
        Some("V152") => Profile::V152,
        _ => Profile::Standard,
    }
}
