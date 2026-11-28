// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

pub struct Tyfza1;

impl PlatformAdapter for Tyfza1 {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::TianyiTyfza1,
        vendor: "Tianyi",
        model: "TYFZA1",
        // The reference project is design evidence, not this device's DTS proof.
        device_tree_compatibles: &[],
        board_names: &[],
        identification: IdentificationStatus::PendingHardwareEvidence,
        implementation: PlatformImplementation::CompileableStub,
    };
}
