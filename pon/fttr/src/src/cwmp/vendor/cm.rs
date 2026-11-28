use super::super::contract::*;

// Candidate only: no onsite CMCC write evidence. Names and layout require discovery.
pub const BASE: Contract = Contract {
    supported_services: &["INTERNET", "OTHER", "IPTV", "VOIP"],
    id: "vendor-cm-candidate",
    vendor: "cm",
    writes_verified: false,
    capability_groups: UNVERIFIED,
    references: None,
    requirements: OperationRequirements {
        disable_wan_before_write: true,
        settle_seconds: 0,
    },
    service_encoding: None,
    wan: WanContract {
        service: services(connection("X_CMCC_ServiceList")),
        vlan: unsigned(connection("X_CMCC_VLANID")),
        enabled: None,
        priority: unsigned(connection("X_CMCC_802-1p")),
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
