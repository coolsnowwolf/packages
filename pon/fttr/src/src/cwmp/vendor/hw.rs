use super::super::contract::*;

pub const BASE: Contract = Contract {
    supported_services: &["INTERNET", "OTHER", "VOIP"],
    id: "vendor-hw-connection-vlan",
    vendor: "hw",
    writes_verified: false,
    capability_groups: UNVERIFIED,
    references: None,
    requirements: OperationRequirements {
        disable_wan_before_write: true,
        settle_seconds: 0,
    },
    service_encoding: None,
    wan: WanContract {
        service: services(connection("X_HW_SERVICELIST")),
        vlan: unsigned(connection("X_HW_VLAN")),
        enabled: None,
        priority: unsigned(connection("X_HW_PRI")),
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
