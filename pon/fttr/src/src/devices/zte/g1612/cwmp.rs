use crate::cwmp::{
    contract::{Capabilities, Contract, OperationRequirements, Support, WlanContract},
    vendor::ct,
};

// ZTE ZXHN G1612 China Telecom FTTR sub-gateway contract.
pub const CONTRACT: Contract = Contract {
    id: "zte-g1612-ct",
    vendor: "ct",
    writes_verified: true,
    wlan: WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        wpa2_beacon: "11i",
        wpa_wpa2_beacon: "WPAand11i",
        nat_writable_exception: false,
        passphrase_fallback: false,
    },
    capability_groups: Capabilities {
        wan: Support::Supported,
        wlan: Support::Supported,
        binding: Support::Unsupported,
    },
    requirements: OperationRequirements {
        disable_wan_before_write: false,
        settle_seconds: 0,
    },
    ..ct::BASE
};
