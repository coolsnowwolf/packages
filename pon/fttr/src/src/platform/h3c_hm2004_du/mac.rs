//! FMCS downstream control register interpretation belongs to this platform.
use crate::platform::resources::PonMacReadback;
pub(super) fn set_downstream(tx: bool, fec: bool) -> std::io::Result<PonMacReadback> {
    let value = super::fmcs::set_gpon_downstream(tx, fec)?;
    Ok(PonMacReadback {
        raw: Some(value),
        tx_enabled: Some(value & 2 != 0),
        fec_enabled: Some(value & 8 != 0),
    })
}
