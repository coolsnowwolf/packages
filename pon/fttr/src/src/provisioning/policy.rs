//! WAN authorization and fallback classification, independent of scheduling.
#[cfg(feature = "acs")]
use super::wan;

#[cfg(feature = "acs")]
pub(crate) fn owns_business_wan(
    device: &str,
    parent: u32,
    connection: Option<(&str, u32)>,
) -> bool {
    wan::load(device).ok().flatten().is_some_and(|owned| {
        owned.values().any(|o| {
            o.parent == parent
                && connection.is_none_or(|(kind, n)| o.kind == kind && o.connection == n)
                && !o.service_list.is_empty()
                && !contains_service_token(&o.service_list, "TR069")
        })
    })
}

#[cfg(feature = "acs")]
pub(crate) fn protected_wan_parent(device: &str, parent: u32) -> bool {
    wan::parent_protected(device, parent)
}

pub(super) fn protected(service: &str, vlan_enabled: bool) -> bool {
    contains_service_token(service, "TR069")
        || (!vlan_enabled && contains_service_token(service, "INTERNET"))
}

pub(crate) fn contains_service_token(service: &str, token: &str) -> bool {
    crate::cwmp::codec::service_tokens(service).any(|value| value.eq_ignore_ascii_case(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwtc_combined_services_preserve_management_and_fallback_protection() {
        assert!(contains_service_token("TR069_INTERNET", "TR069"));
        assert!(contains_service_token("internet_tr069", "INTERNET"));
        assert!(protected("TR069_INTERNET", true));
        assert!(protected("INTERNET_OTHER", false));
        assert!(protected(" IPTV, TR069 ", true));
        assert!(!contains_service_token("NOTTR069", "TR069"));
        assert!(!protected("OTHER", true));
    }
}
