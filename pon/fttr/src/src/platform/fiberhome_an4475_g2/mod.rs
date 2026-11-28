// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod activation;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod backend;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod calibration;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) mod diagnostics;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod downstream;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod gdba;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod health;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod initialization;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod interrupts;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod management;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod olt;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod omci;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod optical;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod polling;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod port_gate;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod profile;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod protocol;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) mod runtime;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod serdes_apb;
#[cfg(all(test, feature = "platform-fiberhome-an4475-g2"))]
mod test_support;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod transceiver;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod transport;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod ubus;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod upstream;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) mod vport;

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub const ONU_COUNT: usize = 128;

#[cfg(feature = "platform-fiberhome-an4475-g2")]
const ONU_IDS: [u8; ONU_COUNT] = {
    let mut ids = [0; ONU_COUNT];
    let mut index = 0;
    while index < ONU_COUNT {
        ids[index] = index as u8;
        index += 1;
    }
    ids
};

#[cfg(feature = "platform-fiberhome-an4475-g2")]
const DOWNSTREAM_INTERFACES: [&str; ONU_COUNT] = [
    "fttr1", "fttr2", "fttr3", "fttr4", "fttr5", "fttr6", "fttr7", "fttr8", "fttr9", "fttr10",
    "fttr11", "fttr12", "fttr13", "fttr14", "fttr15", "fttr16", "fttr17", "fttr18", "fttr19",
    "fttr20", "fttr21", "fttr22", "fttr23", "fttr24", "fttr25", "fttr26", "fttr27", "fttr28",
    "fttr29", "fttr30", "fttr31", "fttr32", "fttr33", "fttr34", "fttr35", "fttr36", "fttr37",
    "fttr38", "fttr39", "fttr40", "fttr41", "fttr42", "fttr43", "fttr44", "fttr45", "fttr46",
    "fttr47", "fttr48", "fttr49", "fttr50", "fttr51", "fttr52", "fttr53", "fttr54", "fttr55",
    "fttr56", "fttr57", "fttr58", "fttr59", "fttr60", "fttr61", "fttr62", "fttr63", "fttr64",
    "fttr65", "fttr66", "fttr67", "fttr68", "fttr69", "fttr70", "fttr71", "fttr72", "fttr73",
    "fttr74", "fttr75", "fttr76", "fttr77", "fttr78", "fttr79", "fttr80", "fttr81", "fttr82",
    "fttr83", "fttr84", "fttr85", "fttr86", "fttr87", "fttr88", "fttr89", "fttr90", "fttr91",
    "fttr92", "fttr93", "fttr94", "fttr95", "fttr96", "fttr97", "fttr98", "fttr99", "fttr100",
    "fttr101", "fttr102", "fttr103", "fttr104", "fttr105", "fttr106", "fttr107", "fttr108",
    "fttr109", "fttr110", "fttr111", "fttr112", "fttr113", "fttr114", "fttr115", "fttr116",
    "fttr117", "fttr118", "fttr119", "fttr120", "fttr121", "fttr122", "fttr123", "fttr124",
    "fttr125", "fttr126", "fttr127", "fttr128",
];

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub const RESOURCES: super::resources::Resources = super::resources::Resources {
    onu_ids: &ONU_IDS,
    management_interfaces: &["fttr0"],
    capture_interface: Some("fttr0"),
    downstream_interfaces: &DOWNSTREAM_INTERFACES,
    allocations: &allocations(),
};

#[cfg(feature = "platform-fiberhome-an4475-g2")]
static ACTIVE_RESOURCES: std::sync::OnceLock<super::resources::Resources> =
    std::sync::OnceLock::new();

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) fn resources_for_count(count: usize) -> super::resources::Resources {
    super::resources::Resources {
        onu_ids: &RESOURCES.onu_ids[..count],
        downstream_interfaces: &RESOURCES.downstream_interfaces[..count],
        allocations: &RESOURCES.allocations[..count],
        ..RESOURCES
    }
}

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) fn active_resources() -> &'static super::resources::Resources {
    ACTIVE_RESOURCES.get().unwrap_or(&RESOURCES)
}

#[cfg(feature = "platform-fiberhome-an4475-g2")]
fn configure_resources() -> Result<(), String> {
    let config = crate::provisioning::load_config()?;
    let count = usize::from(config.fiberhome_onu_count.unwrap_or(ONU_COUNT as u8));
    let resources = resources_for_count(count);
    resources.validate_pins(&config.onu_pins)?;
    ACTIVE_RESOURCES
        .set(resources)
        .map_err(|_| "AN4475 resources already configured".to_string())
}

#[cfg(feature = "platform-fiberhome-an4475-g2")]
const fn allocations() -> [super::resources::OnuResources; ONU_COUNT] {
    let mut entries = [super::resources::OnuResources {
        onu_id: 0,
        alloc_id: 0,
        gem_id: 0,
        interface: None,
    }; ONU_COUNT];
    let mut index = 0;
    while index < entries.len() {
        entries[index] = super::resources::OnuResources {
            onu_id: index as u8,
            // FSL hardware maps data Alloc-IDs in 0x400..(0x500 - max_onus) for alloc_offset_mode=0.
            alloc_id: 0x400 + index as u16,
            gem_id: 0x100 + index as u16,
            interface: Some(DOWNSTREAM_INTERFACES[index]),
        };
        index += 1;
    }
    entries
}

#[derive(Debug)]
pub struct An4475G2;

impl PlatformAdapter for An4475G2 {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::FiberhomeAn4475G2,
        vendor: "FiberHome",
        model: "AN4475-G2",
        // Exact PonWrt board DTS identity, not the original generic AN7581 EVB ID.
        // This confirms source identity only, not live wiring or hardware readiness.
        device_tree_compatibles: &["fiberhome,an4475-g2"],
        board_names: &["fiberhome,an4475-g2"],
        identification: IdentificationStatus::Verified,
        implementation: PlatformImplementation::Runtime,
    };
}

#[cfg(all(test, feature = "platform-fiberhome-an4475-g2"))]
mod resource_tests {
    use super::*;

    #[test]
    fn configured_prefix_preserves_slot_mappings() {
        for count in [1, 31, 32, 127, 128] {
            let resources = resources_for_count(count);
            assert_eq!(resources.onu_ids.len(), count);
            assert_eq!(resources.downstream_interfaces.len(), count);
            assert_eq!(resources.allocations.len(), count);
            assert_eq!(
                resources.onu((count - 1) as u8).unwrap().alloc_id,
                0x400 + count as u16 - 1
            );
            assert!(resources.onu(count as u8).is_err());
            resources.validate().unwrap();
        }
    }

    #[test]
    fn all_slots_have_distinct_valid_allocations_and_interfaces() {
        for id in 0..128u8 {
            let entry = RESOURCES.onu(id).unwrap();
            assert_eq!(entry.alloc_id, 0x400 + u16::from(id));
            assert_eq!(entry.gem_id, 0x100 + u16::from(id));
            assert_eq!(
                entry.interface.unwrap(),
                format!("fttr{}", u16::from(id) + 1)
            );
        }
        assert!(RESOURCES.onu(128).is_err());
    }
}
