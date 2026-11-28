use crate::cwmp::{contract::*, vendor::hw};

// Preserve existing HWTC WAN/WLAN behavior. Binding remains unavailable.
pub const CONTRACT: Contract = Contract {
    id: "hwtc-hw",
    writes_verified: true,
    wlan: WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        ..hw::BASE.wlan
    },
    capability_groups: Capabilities {
        wan: Support::Supported,
        wlan: Support::Supported,
        binding: Support::Unverified,
    },
    ..hw::BASE
};

// V152 exposes individual unsignedInt 0/1 switches, not a reference string.
pub const V152_CONTRACT: Contract = Contract {
    id: "hwtc-v152-hw",
    references: Some(ReferenceEncoding::EndpointFlags {
        lan: &[
            unsigned(connection("X_HW_LANBIND.Lan1Enable")),
            unsigned(connection("X_HW_LANBIND.Lan2Enable")),
            unsigned(connection("X_HW_LANBIND.Lan3Enable")),
            unsigned(connection("X_HW_LANBIND.Lan4Enable")),
        ],
        wlan: &[
            unsigned(connection("X_HW_LANBIND.SSID1Enable")),
            unsigned(connection("X_HW_LANBIND.SSID2Enable")),
            unsigned(connection("X_HW_LANBIND.SSID3Enable")),
            unsigned(connection("X_HW_LANBIND.SSID4Enable")),
            unsigned(connection("X_HW_LANBIND.SSID5Enable")),
            unsigned(connection("X_HW_LANBIND.SSID6Enable")),
            unsigned(connection("X_HW_LANBIND.SSID7Enable")),
            unsigned(connection("X_HW_LANBIND.SSID8Enable")),
        ],
    }),
    wan: WanContract {
        binding: Some(unsigned(connection("X_HW_LANBIND.Lan1Enable"))),
        ..CONTRACT.wan
    },
    capability_groups: Capabilities {
        binding: Support::Supported,
        ..CONTRACT.capability_groups
    },
    ..CONTRACT
};

// K155-11 is a pure bridge FTTR sub-gateway with hardware-bridged SSIDs; no LAN/SSID binding switches.
pub const K155_CONTRACT: Contract = Contract {
    id: "hwtc-k155-hw",
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
    ..hw::BASE
};
