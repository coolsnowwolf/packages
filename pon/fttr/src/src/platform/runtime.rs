// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::{error::Error, fmt, fs, str};

use super::{
    adapter::{
        IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId,
        PlatformImplementation,
    },
    fiberhome_an4475_g2::An4475G2,
    h3c_hm2004_du::Hm2004Du,
    tianyi_tyfza1::Tyfza1,
};

const DEVICE_TREE_COMPATIBLE: &str = "/proc/device-tree/compatible";
const BOARD_NAME: &str = "/tmp/sysinfo/board_name";

const fn descriptors() -> [PlatformDescriptor; 3] {
    [
        Hm2004Du::DESCRIPTOR,
        An4475G2::DESCRIPTOR,
        Tyfza1::DESCRIPTOR,
    ]
}

pub(crate) fn is_compiled(id: PlatformId) -> bool {
    match id {
        PlatformId::H3cHm2004Du => cfg!(feature = "platform-h3c-hm2004-du"),
        PlatformId::FiberhomeAn4475G2 => cfg!(feature = "platform-fiberhome-an4475-g2"),
        PlatformId::TianyiTyfza1 => cfg!(feature = "platform-tianyi-tyfza1"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    UnknownPlatform,
    Ambiguous(Vec<PlatformId>),
    PlatformNotCompiled(PlatformId),
    IdentificationPending(PlatformId),
    BackendNotImplemented(PlatformId),
    InvalidResources(PlatformId, String),
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlatform => {
                write!(f, "device identity does not match a supported platform")
            }
            Self::Ambiguous(ids) => {
                write!(f, "device identity matches multiple platforms: {ids:?}")
            }
            Self::PlatformNotCompiled(id) => {
                write!(
                    f,
                    "platform {} was identified but not compiled into this image",
                    id.as_str()
                )
            }
            Self::IdentificationPending(id) => {
                write!(
                    f,
                    "runtime identifiers for platform {} are not verified",
                    id.as_str()
                )
            }
            Self::BackendNotImplemented(id) => {
                write!(
                    f,
                    "platform {} is recognized but has no hardware backend",
                    id.as_str()
                )
            }
            Self::InvalidResources(id, reason) => {
                write!(
                    f,
                    "platform {} has invalid resources: {reason}",
                    id.as_str()
                )
            }
        }
    }
}

impl Error for SelectionError {}

/// Resolve exact OpenWrt board name and device-tree compatible values.
/// Values from the two sources must agree if both identify a known platform.
pub fn identify_from_values(
    board_name: Option<&str>,
    device_tree_compatible: &[u8],
) -> Result<PlatformId, SelectionError> {
    identify_with_descriptors(board_name, device_tree_compatible, &descriptors())
}

fn identify_with_descriptors(
    board_name: Option<&str>,
    device_tree_compatible: &[u8],
    descriptors: &[PlatformDescriptor],
) -> Result<PlatformId, SelectionError> {
    identify_for_purpose(board_name, device_tree_compatible, descriptors, false)
}

/// Same exact identity/compilation checks as runtime selection, but permits a
/// diagnostic-only implementation. This never authorizes starting the daemon.
pub fn identify_diagnostics_from_values(
    board_name: Option<&str>,
    device_tree_compatible: &[u8],
) -> Result<PlatformId, SelectionError> {
    identify_for_purpose(board_name, device_tree_compatible, &descriptors(), true)
}

fn identify_for_purpose(
    board_name: Option<&str>,
    device_tree_compatible: &[u8],
    descriptors: &[PlatformDescriptor],
    diagnostics: bool,
) -> Result<PlatformId, SelectionError> {
    let board_name =
        board_name.map(|value| value.trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace()));
    let compatible_values: Vec<&str> = device_tree_compatible
        .split(|byte| *byte == 0)
        .filter_map(|value| str::from_utf8(value).ok())
        .collect();

    let matches: Vec<PlatformDescriptor> = descriptors
        .iter()
        .copied()
        .filter(|descriptor| {
            board_name.is_some_and(|name| descriptor.board_names.contains(&name))
                || compatible_values
                    .iter()
                    .any(|value| descriptor.device_tree_compatibles.contains(value))
        })
        .collect();

    let [descriptor] = matches.as_slice() else {
        return if matches.is_empty() {
            Err(SelectionError::UnknownPlatform)
        } else {
            Err(SelectionError::Ambiguous(
                matches.iter().map(|descriptor| descriptor.id).collect(),
            ))
        };
    };

    if !is_compiled(descriptor.id) {
        return Err(SelectionError::PlatformNotCompiled(descriptor.id));
    }
    if descriptor.identification != IdentificationStatus::Verified {
        return Err(SelectionError::IdentificationPending(descriptor.id));
    }
    if descriptor.implementation != PlatformImplementation::Runtime
        && !(diagnostics && descriptor.implementation == PlatformImplementation::DiagnosticsOnly)
    {
        return Err(SelectionError::BackendNotImplemented(descriptor.id));
    }

    Ok(descriptor.id)
}

pub fn detect() -> Result<PlatformId, SelectionError> {
    detect_for_purpose(false)
}

pub fn detect_diagnostics() -> Result<PlatformId, SelectionError> {
    detect_for_purpose(true)
}

fn detect_for_purpose(diagnostics: bool) -> Result<PlatformId, SelectionError> {
    let board = fs::read(BOARD_NAME).ok();
    let compatible = fs::read(DEVICE_TREE_COMPATIBLE).unwrap_or_default();
    let board_name = board
        .as_deref()
        .and_then(|bytes| str::from_utf8(bytes).ok());
    if diagnostics {
        identify_diagnostics_from_values(board_name, &compatible)
    } else {
        identify_from_values(board_name, &compatible)
    }
}

/// Read-only build/runtime summary for support diagnostics.
pub fn report_json() -> String {
    let platforms = descriptors()
        .into_iter()
        .map(|descriptor| {
            serde_json::json!({
                "id": descriptor.id.as_str(),
                "vendor": descriptor.vendor,
                "model": descriptor.model,
                "compiled": is_compiled(descriptor.id),
                "identification": match descriptor.identification {
                    IdentificationStatus::Verified => "verified",
                    IdentificationStatus::PendingHardwareEvidence => "pending-hardware-evidence",
                },
                "implementation": match descriptor.implementation {
                    PlatformImplementation::Runtime => "runtime",
                    PlatformImplementation::DiagnosticsOnly => "diagnostics-only",
                    PlatformImplementation::CompileableStub => "stub",
                },
            })
        })
        .collect::<Vec<_>>();
    let selected = super::context::PlatformContext::detect();
    let diagnostic = detect_diagnostics();
    serde_json::json!({
        "schema_version": 1,
        "platforms": platforms,
        "selected_platform": selected.as_ref().ok().map(|platform| platform.id().as_str()),
        "selection_ready": selected.is_ok(),
        "diagnostic_platform": diagnostic.as_ref().ok().map(|id| id.as_str()),
        "diagnostic_selection_ready": diagnostic.is_ok(),
        "runtime_ready": false,
        "hardware_ready": null,
        "selection_error": selected.err().map(|error| error.to_string()),
    })
    .to_string()
}

pub fn run() -> Result<(), SelectionError> {
    run_selected(super::context::PlatformContext::detect()?)
}

pub fn run_selected(platform: super::context::PlatformContext) -> Result<(), SelectionError> {
    platform.backend().run(platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_h3c_board_and_device_tree_identity_select_existing_runtime() {
        let expected = if cfg!(feature = "platform-h3c-hm2004-du") {
            Ok(PlatformId::H3cHm2004Du)
        } else {
            Err(SelectionError::PlatformNotCompiled(PlatformId::H3cHm2004Du))
        };
        assert_eq!(
            identify_from_values(Some("h3c,hm2004-du\n"), b"vendor,chip\0h3c,hm2004-du\0"),
            expected
        );
        assert_eq!(identify_from_values(Some("h3c,hm2004-du"), b""), expected);
        assert_eq!(
            identify_from_values(None, b"vendor,chip\0h3c,hm2004-du\0"),
            expected
        );
    }

    #[test]
    fn unknown_and_substring_identities_fail_closed() {
        assert_eq!(
            identify_from_values(Some("vendor,hm2004-du-rev2"), b"vendor,hm2004-du"),
            Err(SelectionError::UnknownPlatform)
        );
        assert_eq!(
            identify_from_values(None, b"h3c,hm2004-du-extra\0"),
            Err(SelectionError::UnknownPlatform)
        );
    }

    #[test]
    fn conflicting_identity_sources_are_ambiguous() {
        let mut fiberhome = An4475G2::DESCRIPTOR;
        // Synthetic identity to isolate ambiguity handling from board metadata.
        fiberhome.device_tree_compatibles = &["fiberhome,test-board"];
        assert_eq!(
            identify_with_descriptors(
                Some("h3c,hm2004-du"),
                b"vendor,chip\0fiberhome,test-board\0",
                &[Hm2004Du::DESCRIPTOR, fiberhome],
            ),
            Err(SelectionError::Ambiguous(vec![
                PlatformId::H3cHm2004Du,
                PlatformId::FiberhomeAn4475G2,
            ]))
        );
    }

    #[test]
    fn recognized_stub_never_dispatches_to_h3c_runtime() {
        let mut fiberhome = An4475G2::DESCRIPTOR;
        // Synthetic stub to test rejection independently of diagnostic support.
        fiberhome.board_names = &["fiberhome,test-board"];
        fiberhome.identification = IdentificationStatus::Verified;
        fiberhome.implementation = PlatformImplementation::CompileableStub;
        let expected = if cfg!(feature = "platform-fiberhome-an4475-g2") {
            Err(SelectionError::BackendNotImplemented(
                PlatformId::FiberhomeAn4475G2,
            ))
        } else {
            Err(SelectionError::PlatformNotCompiled(
                PlatformId::FiberhomeAn4475G2,
            ))
        };
        assert_eq!(
            identify_with_descriptors(Some("fiberhome,test-board"), b"", &[fiberhome]),
            expected
        );
    }

    #[test]
    fn fiberhome_diagnostics_never_authorize_generic_evb() {
        let id = PlatformId::FiberhomeAn4475G2;
        let expected = if cfg!(feature = "platform-fiberhome-an4475-g2") {
            Ok(id)
        } else {
            Err(SelectionError::PlatformNotCompiled(id))
        };
        assert_eq!(
            identify_diagnostics_from_values(None, b"fiberhome,an4475-g2\0"),
            expected
        );
        assert_eq!(
            identify_from_values(Some("fiberhome,an4475-g2"), b""),
            expected
        );
        assert_eq!(
            identify_diagnostics_from_values(None, b"airoha,an7581-evb\0airoha,an7581\0"),
            Err(SelectionError::UnknownPlatform)
        );
        assert_eq!(
            super::super::context::PlatformContext::resolve(id).is_ok(),
            cfg!(feature = "platform-fiberhome-an4475-g2")
        );
    }

    #[test]
    fn compile_feature_sets_allow_arbitrary_platform_subsets() {
        let h3c = identify_from_values(Some("h3c,hm2004-du"), b"");
        if cfg!(feature = "platform-h3c-hm2004-du") {
            assert_eq!(h3c, Ok(PlatformId::H3cHm2004Du));
        } else {
            assert_eq!(
                h3c,
                Err(SelectionError::PlatformNotCompiled(PlatformId::H3cHm2004Du))
            );
        }
    }

    #[test]
    fn info_report_contains_only_platform_metadata_and_readiness() {
        let report: serde_json::Value = serde_json::from_str(&report_json()).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["platforms"].as_array().unwrap().len(), 3);
        assert!(report["runtime_ready"].is_boolean());
        assert!(report.get("serial_number").is_none());
    }
}
