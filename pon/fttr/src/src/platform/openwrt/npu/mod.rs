//! Platform-aware Airoha NPU/PPE offload admission and diagnostics.
//!
//! Supported hardware admission models:
//! - `PortMask` (H3C HM2004-DU): `h3c_fmcs` per-port masks + `airoha_eth` `fttr_offload_directions`.
//! - `DirectionOnly` (FiberHome AN4475-G2): `airoha_eth` `fttr_offload_directions` + 802.1ad VPORTs.

pub mod command;
pub mod ppe;
pub mod reconciler;
pub mod service;
pub mod settings;

use crate::model::NpuStatus;
use crate::platform::adapter::PlatformId;
use fttr_api::NpuAdmissionModel;
use std::{io, path::Path, time::Duration};

pub use ppe::{parse_ppe_entry, status_from, status_from_direction_only};
pub use reconciler::{
    hooks_ready, routed_hooks_ready, text_offload_flag, topology, Reconciler, Topology,
};
pub use service::{
    active_admission_model, admission_model_for, is_supported_platform, Service, ServiceHandle,
};

fn check_hooks(status: &NpuStatus, model: NpuAdmissionModel) -> io::Result<bool> {
    let routed = status.wan_upstream || status.wan_downstream;
    // The BSP registers its bridge forward hook in the driver.
    let bridge = status.onu_bridge && status.bsp_ppe.is_none();
    let mut all_ready = true;
    if routed {
        match model {
            NpuAdmissionModel::PortMask => {
                if !status.routed_mask_valid {
                    return Err(io::Error::other("routed port mask unavailable"));
                }
            }
            NpuAdmissionModel::DirectionOnly => {
                if !status.direction_mask_valid {
                    return Err(io::Error::other("FTTR direction mask unavailable"));
                }
            }
        }
        let json = command::run_sync(
            "nft",
            &["-j", "list", "flowtable", "inet", "fw4", "ft"],
            Duration::from_secs(2),
        )?;
        let table: serde_json::Value = serde_json::from_str(&json).map_err(io::Error::other)?;
        let has_flags = table["nftables"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["flowtable"].get("flags").is_some())
        });
        let text = if has_flags {
            String::new()
        } else {
            command::run_sync(
                "nft",
                &["list", "flowtable", "inet", "fw4", "ft"],
                Duration::from_secs(2),
            )?
        };
        if !reconciler::routed_hooks_ready(&table, &text) {
            all_ready = false;
        }
    }
    if bridge {
        match model {
            NpuAdmissionModel::PortMask => {
                if !status.mask_valid {
                    return Err(io::Error::other("bridge port mask unavailable"));
                }
            }
            NpuAdmissionModel::DirectionOnly => {
                if !status.direction_mask_valid {
                    return Err(io::Error::other("FTTR direction mask unavailable"));
                }
            }
        }
        let json = command::run_sync(
            "nft",
            &["-j", "list", "flowtable", "bridge", "fw4", "fb"],
            Duration::from_secs(2),
        )?;
        let table: serde_json::Value = serde_json::from_str(&json).map_err(io::Error::other)?;
        let ports = status
            .active_ports
            .iter()
            .map(|p| (p.clone(), Vec::new()))
            .collect::<Vec<_>>();
        let has_flags = table["nftables"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["flowtable"].get("flags").is_some())
        });
        let text = if has_flags {
            String::new()
        } else {
            command::run_sync(
                "nft",
                &["list", "flowtable", "bridge", "fw4", "fb"],
                Duration::from_secs(2),
            )?
        };
        if !hooks_ready(&table, &ports, &text) {
            all_ready = false;
        }
    }
    Ok(all_ready)
}

/// Synchronous NPU status query used by fttrctl, ubus, and snapshot telemetry.
pub fn status() -> NpuStatus {
    let Some(model) = active_admission_model() else {
        return NpuStatus::default();
    };
    status_for_model(model)
}

pub(crate) fn status_for_platform(id: PlatformId) -> NpuStatus {
    let Some(model) = admission_model_for(id) else {
        return NpuStatus::default();
    };
    status_for_model(model)
}

fn status_for_model(model: NpuAdmissionModel) -> NpuStatus {
    let mut status = match model {
        NpuAdmissionModel::PortMask => {
            let bsp_driver = Path::new("/sys/bus/platform/drivers/an7581-bsp-ppe");
            let mut status = ppe::status_from_h3c(
                Path::new("/etc/config/fttr"),
                Path::new("/sys/kernel/debug/ppe"),
                Path::new("/sys/module/h3c_fmcs/parameters"),
                bsp_driver.is_dir().then_some((
                    bsp_driver,
                    Path::new("/sys/module/an7581_bsp_ppe/parameters"),
                )),
            );
            ppe::populate_optional_direction_mask(
                &mut status,
                Path::new("/sys/module/airoha_eth/parameters"),
            );
            status
        }
        NpuAdmissionModel::DirectionOnly => status_from_direction_only(
            Path::new("/etc/config/fttr"),
            Path::new("/sys/kernel/debug/ppe"),
            Path::new("/sys/module/airoha_eth/parameters"),
            Path::new("/sys/class/net"),
        ),
    };

    match check_hooks(&status, model) {
        Ok(ready) => {
            status.hooks_valid = true;
            status.fw4_hooks_ready = ready;
        }
        Err(error) => status.errors.push(format!("fw4 hooks: {error}")),
    }

    status
}
