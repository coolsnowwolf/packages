//! SG631Z-C identity rules. OMCI uses the generic bridge; no business CWMP contract.
pub(super) fn matches_cwmp(optical_sn: &str, serial: &str) -> bool {
    // SG631Z-C V3.0.2 reports a compound CWMP serial containing its full optical SN.
    // The vendor dispatcher checks vendor/model; discovery rejects ambiguity.
    optical_sn.len() == 12
        && optical_sn.starts_with("STAR")
        && optical_sn[4..].bytes().all(|b| b.is_ascii_hexdigit())
        && serial.contains(optical_sn)
}
