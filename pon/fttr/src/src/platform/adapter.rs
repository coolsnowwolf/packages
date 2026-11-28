// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

/// Stable identity for a gateway/OLT platform supported by this userspace.
/// This is separate from `devices::*`, which describes downstream ONUs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformId {
    H3cHm2004Du,
    FiberhomeAn4475G2,
    TianyiTyfza1,
}

impl PlatformId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::H3cHm2004Du => "h3c-hm2004-du",
            Self::FiberhomeAn4475G2 => "fiberhome-an4475-g2",
            Self::TianyiTyfza1 => "tianyi-tyfza1",
        }
    }
}

/// What is currently implemented for this platform in the Rust userspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformImplementation {
    /// Hardware operations and a runtime entry are implemented by the platform.
    Runtime,
    /// Raw diagnostics exist; initialization and the full ONU runtime do not.
    DiagnosticsOnly,
    /// Identity-only adapter. No platform driver or hardware operations exist.
    CompileableStub,
}

/// Whether the exact runtime identifiers have been confirmed from device data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentificationStatus {
    Verified,
    PendingHardwareEvidence,
}

/// Descriptive metadata used by platform registration and runtime selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformDescriptor {
    pub id: PlatformId,
    pub vendor: &'static str,
    pub model: &'static str,
    /// Exact device-tree `compatible` values. Empty until verified from DTS.
    pub device_tree_compatibles: &'static [&'static str],
    /// Exact OpenWrt board names, when verified. Never match by substring.
    pub board_names: &'static [&'static str],
    pub identification: IdentificationStatus,
    pub implementation: PlatformImplementation,
}

/// Compile-time identity metadata. Hardware operations implement PlatformBackend.
pub trait PlatformAdapter {
    const DESCRIPTOR: PlatformDescriptor;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        fiberhome_an4475_g2::An4475G2, h3c_hm2004_du::Hm2004Du, tianyi_tyfza1::Tyfza1,
    };

    #[test]
    fn h3c_descriptor_matches_existing_board_gate() {
        let descriptor = Hm2004Du::DESCRIPTOR;
        assert_eq!(descriptor.id, PlatformId::H3cHm2004Du);
        assert_eq!(descriptor.device_tree_compatibles, &["h3c,hm2004-du"]);
        assert_eq!(descriptor.board_names, &["h3c,hm2004-du"]);
        assert_eq!(descriptor.identification, IdentificationStatus::Verified);
        assert_eq!(descriptor.implementation, PlatformImplementation::Runtime);
    }

    #[test]
    fn unresearched_platforms_have_no_guessed_runtime_identifiers() {
        for descriptor in [Tyfza1::DESCRIPTOR] {
            assert!(descriptor.device_tree_compatibles.is_empty());
            assert!(descriptor.board_names.is_empty());
            assert_eq!(
                descriptor.identification,
                IdentificationStatus::PendingHardwareEvidence
            );
            assert_eq!(
                descriptor.implementation,
                PlatformImplementation::CompileableStub
            );
        }
    }

    #[test]
    fn fiberhome_source_identity_advertises_runtime() {
        let descriptor = An4475G2::DESCRIPTOR;
        assert_eq!(descriptor.device_tree_compatibles, &["fiberhome,an4475-g2"]);
        assert_eq!(descriptor.board_names, &["fiberhome,an4475-g2"]);
        assert_eq!(descriptor.implementation, PlatformImplementation::Runtime);
    }
}
