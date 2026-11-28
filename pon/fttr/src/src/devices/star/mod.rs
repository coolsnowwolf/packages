//! STAR vendor dispatch. Model-specific behavior belongs in its model module.
mod sg631;

pub(super) fn matches_cwmp(optical_sn: &str, product_class: &str, serial: &str) -> bool {
    if !optical_sn.starts_with("STAR") {
        return false;
    }
    match product_class {
        "SG631Z-C" => sg631::matches_cwmp(optical_sn, serial),
        _ => false,
    }
}
