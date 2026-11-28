use crate::cwmp::{contract::*, vendor::cu};

pub const CONTRACT: Contract = Contract {
    id: "hl202-cu",
    writes_verified: true,
    capability_groups: Capabilities {
        wan: Support::Supported,
        wlan: Support::Supported,
        binding: Support::Supported,
    },
    references: Some(ReferenceEncoding::FullPathsComma),
    service_encoding: Some(crate::cwmp::codec::ServiceEncoding::Comma),
    wan: WanContract {
        binding: Some(ignore_wlan(
            reference(connection("X_CU_LanInterface")),
            // Firmware appends these non-addressable vendor WLAN references.
            // Actual, discoverable WLAN slots remain 1..=8 and are compared exactly.
            &[9, 10, 13, 14],
        )),
        cloud_cpe: Some(boolean(connection("X_CU_CloudCPEEnable"))),
        ..cu::BASE.wan
    },
    wlan: WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        wpa2_beacon: "WPA2",
        wpa_wpa2_beacon: "WPA/WPA2",
        nat_writable_exception: true,
        passphrase_fallback: true,
    },
    ..cu::BASE
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_ignores_phantom_references_but_requires_exact_real_wlan_set() {
        let field = CONTRACT.wan.binding.unwrap();
        let wlan = "InternetGatewayDevice.LANDevice.1.WLANConfiguration";
        let desired = format!("{wlan}.1,{wlan}.5");
        assert!(equivalent_field(
            field,
            &desired,
            &format!("{desired},{wlan}.9,{wlan}.13")
        ));
        assert!(!equivalent_field(
            field,
            &desired,
            &format!("{desired},{wlan}.2")
        ));
        assert!(!equivalent_field(
            field,
            &desired,
            &format!("{wlan}.1,{wlan}.9,{wlan}.13")
        ));
    }
}
