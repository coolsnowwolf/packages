// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

#[derive(Debug)]
pub struct Hm2004Du;

#[cfg(feature = "platform-h3c-hm2004-du")]
mod backend;

#[cfg(all(feature = "platform-h3c-hm2004-du", feature = "device-hl202"))]
use crate::controller::hl202;
#[cfg(feature = "platform-h3c-hm2004-du")]
use crate::controller::{attribute_probe, fec, inventory, lifetime, pon_mac, service, telemetry};
#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod activation;
#[cfg(feature = "platform-h3c-hm2004-du")]
mod dataplane;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub(crate) mod health;
#[cfg(feature = "platform-h3c-hm2004-du")]
mod mac;
#[cfg(all(feature = "npu", feature = "platform-h3c-hm2004-du"))]
pub mod offload;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub(crate) mod runtime;
#[cfg(feature = "platform-h3c-hm2004-du")]
mod status;

#[cfg(feature = "platform-h3c-hm2004-du")]
/// HM2004-DU hardware slots; independent of the GPON protocol ONU-ID limit.
pub const ONU_COUNT: usize = 16;

#[cfg(feature = "platform-h3c-hm2004-du")]
const ONU_IDS: [u8; ONU_COUNT] = {
    let mut ids = [0; ONU_COUNT];
    let mut index = 0;
    while index < ONU_COUNT {
        ids[index] = index as u8;
        index += 1;
    }
    ids
};

#[cfg(feature = "platform-h3c-hm2004-du")]
const DOWNSTREAM_INTERFACES: [&str; ONU_COUNT] = [
    "fttr1", "fttr2", "fttr3", "fttr4", "fttr5", "fttr6", "fttr7", "fttr8", "fttr9", "fttr10",
    "fttr11", "fttr12", "fttr13", "fttr14", "fttr15", "fttr16",
];

#[cfg(feature = "platform-h3c-hm2004-du")]
pub const RESOURCES: super::resources::Resources = super::resources::Resources {
    onu_ids: &ONU_IDS,
    management_interfaces: &["molt_ploam", "molt_omci", "fttr0"],
    capture_interface: Some("fttr0"),
    downstream_interfaces: &DOWNSTREAM_INTERFACES,
    allocations: &allocations(),
};

#[cfg(feature = "platform-h3c-hm2004-du")]
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
            alloc_id: 0x100 + index as u16,
            gem_id: 0x100 + index as u16,
            interface: Some(DOWNSTREAM_INTERFACES[index]),
        };
        index += 1;
    }
    entries
}

#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod calibration;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod diagnostic_cli;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod diagnostics;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod firmware;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub mod fmcs;
#[cfg(feature = "platform-h3c-hm2004-du")]
pub(crate) mod omci;

impl PlatformAdapter for Hm2004Du {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::H3cHm2004Du,
        vendor: "H3C",
        model: "HM2004-DU",
        device_tree_compatibles: &["h3c,hm2004-du"],
        board_names: &["h3c,hm2004-du"],
        identification: IdentificationStatus::Verified,
        implementation: PlatformImplementation::Runtime,
    };
}
