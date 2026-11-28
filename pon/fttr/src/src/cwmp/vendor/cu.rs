use super::super::contract::*;

// This shape comes from the observed HL202 tree, not a universal CU standard.
pub const BASE: Contract = Contract {
    supported_services: &["INTERNET", "OTHER", "IPTV", "VOIP"],
    id: "vendor-cu-parent-vlan",
    vendor: "cu",
    writes_verified: false,
    capability_groups: UNVERIFIED,
    references: None,
    requirements: OperationRequirements {
        disable_wan_before_write: true,
        settle_seconds: 0,
    },
    service_encoding: None,
    wan: WanContract {
        service: services(connection("X_CU_ServiceList")),
        vlan: unsigned(parent("X_CU_VLAN")),
        enabled: Some(boolean(parent("X_CU_VLANEnabled"))),
        priority: unsigned(parent("X_CU_802_1p")),
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
