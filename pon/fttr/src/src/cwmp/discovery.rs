//! Pure matching over read-only GetParameterNames/GetParameterValues evidence.
//! No ACS writes and no task scheduling occur in this module.
use super::contract::*;
#[cfg(test)]
use super::vendor;

#[derive(Clone, Debug)]
pub struct Evidence {
    pub path: String,
    pub writable: Option<bool>,
    pub kind: Option<ScalarType>,
}

pub use super::contract::ValueKind as ScalarType;

#[derive(Debug)]
pub enum Resolution {
    Matched(&'static Contract),
    Ambiguous(Vec<&'static str>),
    Incomplete,
}

/// Compare exact paths in one real parent/connection pair. This avoids merging
/// unrelated instances, accepting wrong object levels, or counting prefix hits.
pub fn match_wan(evidence: &[Evidence], parent: &str, connection: &str) -> Resolution {
    super::matching::match_wan(evidence, parent, connection)
}

/// Exact known identity takes precedence. Parameter validity is checked later
/// by the normal read/discover/write pipeline; it must not trigger silent fallback.
pub fn specialization(optical_vendor: &str, product_class: &str) -> Option<&'static Contract> {
    #[cfg(feature = "device-hwtc")]
    if optical_vendor == "HWTC" && product_class == "V152" {
        return Some(&crate::devices::hwtc::cwmp::V152_CONTRACT);
    }
    #[cfg(feature = "device-hwtc")]
    if optical_vendor == "HWTC" && product_class == "K155-11" {
        return Some(&crate::devices::hwtc::cwmp::K155_CONTRACT);
    }
    #[cfg(feature = "device-hl202")]
    if optical_vendor == "H3CT" && product_class == "HL202-DU-C" {
        return Some(&crate::devices::hl202::cwmp::CONTRACT);
    }
    #[cfg(feature = "device-zte")]
    if (optical_vendor == "9866" || optical_vendor == "ZTEG" || optical_vendor == "ZTE")
        && (product_class == "ZXHN G1612" || product_class == "G1612")
    {
        return Some(&crate::devices::zte::g1612::cwmp::CONTRACT);
    }
    let _ = (optical_vendor, product_class);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const P: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.";
    const C: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.2.";
    fn evidence(c: &Contract) -> Vec<Evidence> {
        let w = c.wan;
        let mut fields = vec![w.service, w.vlan, w.priority];
        fields.extend(w.enabled);
        fields
            .into_iter()
            .map(|f| Evidence {
                path: f.path(P, C),
                writable: Some(true),
                kind: Some(if f.name == w.service.name {
                    ScalarType::String
                } else if w.enabled.is_some_and(|e| e.name == f.name) {
                    ScalarType::Boolean
                } else {
                    ScalarType::UnsignedInt
                }),
            })
            .collect()
    }
    #[test]
    fn exact_paths_select_vendor_but_do_not_authorize_unverified_writes() {
        for c in vendor::candidates() {
            let Resolution::Matched(found) = match_wan(&evidence(c), P, C) else {
                panic!("missing match")
            };
            assert_eq!(found.id, c.id);
            assert!(!found.writes_verified);
        }
    }
    #[test]
    fn aliases_wrong_levels_and_missing_writability_are_not_guessed() {
        let mut values = evidence(&vendor::cu::BASE);
        values.extend(evidence(&vendor::hw::BASE));
        assert!(matches!(match_wan(&values, P, C), Resolution::Ambiguous(_)));
        let mut values = evidence(&vendor::hw::BASE);
        values[1].path = format!("{P}X_HW_VLAN");
        assert!(matches!(match_wan(&values, P, C), Resolution::Incomplete));
        values = evidence(&vendor::hw::BASE);
        values[0].writable = None;
        assert!(matches!(match_wan(&values, P, C), Resolution::Incomplete));
        values = evidence(&vendor::hw::BASE);
        values[1].kind = Some(ScalarType::String);
        assert!(matches!(match_wan(&values, P, C), Resolution::Incomplete));
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn hl202_identity_has_explicit_specialization() {
        assert_eq!(specialization("H3CT", "HL202-DU-C").unwrap().id, "hl202-cu");
        assert!(specialization("H3CT", "HL202-other").is_none());
        assert!(specialization("HWTC", "HL202-DU-C").is_none());
    }
    #[cfg(feature = "device-hwtc")]
    #[test]
    fn hwtc_identity_has_explicit_specialization() {
        assert_eq!(specialization("HWTC", "V152").unwrap().id, "hwtc-v152-hw");
        assert_eq!(
            specialization("HWTC", "K155-11").unwrap().id,
            "hwtc-k155-hw"
        );
        assert!(specialization("HWTC", "other").is_none());
    }
}
