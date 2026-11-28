use super::super::contract::*;

pub fn matches_identity(optical_sn: &str, product_class: &str, serial: &str) -> bool {
    if product_class != "TEWA-768G"
        || optical_sn.len() != 12
        || !optical_sn.starts_with("SCTY")
        || !optical_sn[4..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    // This firmware reports a CT prefix plus its MAC, not the GPON serial.
    serial.eq_ignore_ascii_case(&format!("308618C81{}", &optical_sn[4..]))
}

// Candidate only: presence is not evidence that this firmware supports writes.
pub const BASE: Contract = Contract {
    supported_services: &["INTERNET", "OTHER", "IPTV", "VOIP"],
    id: "vendor-ct-candidate",
    vendor: "ct",
    writes_verified: false,
    capability_groups: UNVERIFIED,
    references: None,
    requirements: OperationRequirements {
        disable_wan_before_write: true,
        settle_seconds: 0,
    },
    service_encoding: None,
    wan: WanContract {
        service: services(connection("X_CT-COM_ServiceList")),
        vlan: unsigned(connection("X_CT-COM_VLANID")),
        enabled: None,
        priority: unsigned(connection("X_CT-COM_802-1p")),
        binding: None,
        cloud_cpe: None,
    },
    wlan: WlanContract {
        radio_slots: None,
        wpa2_beacon: "11i",
        wpa_wpa2_beacon: "WPAand11i",
        nat_writable_exception: false,
        passphrase_fallback: false,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tewa_identity_requires_the_complete_native_serial_and_exact_model() {
        assert!(matches_identity(
            "SCTY72ABCDEF",
            "TEWA-768G",
            "308618C8172ABCDEF"
        ));
        assert!(matches_identity(
            "SCTY72abcdef",
            "TEWA-768G",
            "308618c8172abcdef"
        ));
        for (optical, model, serial) in [
            ("SCTY72ABCDEF", "TEWA-768", "308618C8172ABCDEF"),
            ("HWTC72ABCDEF", "TEWA-768G", "308618C8172ABCDEF"),
            ("SCTY72ABCDE0", "TEWA-768G", "308618C8172ABCDEF"),
            ("SCTY72ABCDEF", "TEWA-768G", "72ABCDEF"),
            ("SCTY72ABCDEF", "TEWA-768G", "308618C8172ABCDEF-extra"),
            ("SCTY72ABCDEF", "TEWA-768G", "308618C8272ABCDEF"),
            ("SCTY72ABCD\u{e9}", "TEWA-768G", "308618C8172ABCDEF"),
        ] {
            assert!(!matches_identity(optical, model, serial));
        }
    }
}
