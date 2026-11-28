//! Compile-time support, deliberately independent of UCI, hardware and liveness.
pub use fttr_api::{AdapterCapability, Capabilities};
use fttr_api::{CapabilityFeatures, CapabilityFunctions, CapabilityPages};
use std::{fs, io, path::Path};

pub fn compiled() -> Capabilities {
    let acs = cfg!(feature = "acs");
    let acs_bridge = cfg!(feature = "acs-bridge");
    let mqtt = cfg!(feature = "mqtt");
    let mqtt_bridge = cfg!(feature = "mqtt-bridge");
    let hl202 = cfg!(feature = "device-hl202");
    let hwtc = cfg!(feature = "device-hwtc");
    let star = cfg!(feature = "device-star");
    let zte = cfg!(feature = "device-zte");
    let scty = cfg!(feature = "device-scty");
    let operations: Vec<_> = crate::platform::backend::compiled_operations().collect();
    let hardware = operations
        .iter()
        .any(|ops| ops.fpga_diagnostics || ops.bosa_diagnostics);
    let optical = operations.iter().any(|ops| ops.optical_control);
    let omci_mib = operations.iter().any(|ops| ops.omci_mib);
    let offload = operations.iter().any(|ops| ops.npu_offload);
    let calibration = operations
        .iter()
        .any(|ops| ops.calibration_profile.is_some());
    let adapters = fttr_api::AdapterCapabilities {
        omci: crate::adapters::omci::available(),
        bootstrap: crate::adapters::bootstrap::available(),
        provisioning: crate::adapters::provisioning::capabilities(),
    };
    Capabilities {
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION"),
        features: CapabilityFeatures {
            acs,
            acs_bridge,
            mqtt,
            mqtt_bridge,
            npu: cfg!(feature = "npu"),
            device_hl202: hl202,
            device_hwtc: hwtc,
            device_star: star,
            device_zte: zte,
            device_scty: scty,
        },
        functions: CapabilityFunctions {
            optical,
            bridge_offload: offload,
            omci_mib,
            mqtt_telemetry: mqtt,
            hl202_management: hl202 && mqtt,
            cwmp: acs,
            command_provisioning: hl202 && mqtt,
        },
        // Configuration and synchronization are the ACS/CWMP workflow in LuCI.
        pages: CapabilityPages {
            status: true,
            hardware,
            calibration,
            about: true,
            config: acs || (hl202 && mqtt),
            sync: acs || (hl202 && mqtt),
            hl202: hl202 && mqtt,
        },
        adapters,
    }
}

pub fn json() -> String {
    serde_json::to_string_pretty(&compiled()).expect("static capabilities serialize")
}

// LuCI's server-side menu only supports filesystem/UCI dependencies. Publish
// page markers from the same capability model, including when MQTT cannot start.
pub fn publish_menu() -> io::Result<()> {
    publish_menu_at(Path::new("/var/run/fttr-capabilities"), Path::new("/tmp"))
}

fn publish_menu_at(directory: &Path, cache_directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let mut changed = false;
    for (page, supported) in compiled().pages.entries() {
        let path = directory.join(page);
        if supported && !path.is_file() {
            fs::write(&path, b"")?;
            changed = true;
        } else if !supported && path.exists() {
            fs::remove_file(&path)?;
            changed = true;
        }
    }
    if changed {
        for entry in fs::read_dir(cache_directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("luci-indexcache.") && name.ends_with(".json") {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_refresh_removes_stale_support_and_invalidates_luci_cache() {
        let root = std::env::temp_dir().join(format!("fttr-capabilities-{}", std::process::id()));
        let markers = root.join("markers");
        fs::create_dir_all(&markers).unwrap();
        // Seed the opposite build's marker set.
        for (page, supported) in compiled().pages.entries() {
            if !supported {
                fs::write(markers.join(page), b"").unwrap();
            }
        }
        let cache = root.join("luci-indexcache.fixture.json");
        fs::write(&cache, b"{}").unwrap();
        let unrelated = root.join("keep.json");
        fs::write(&unrelated, b"{}").unwrap();
        publish_menu_at(&markers, &root).unwrap();
        for (page, supported) in compiled().pages.entries() {
            assert_eq!(markers.join(page).is_file(), supported, "{page}");
        }
        assert!(!cache.exists());
        assert!(unrelated.exists());
        // A repeat does not invalidate an already correct menu.
        fs::write(&cache, b"{}").unwrap();
        publish_menu_at(&markers, &root).unwrap();
        assert!(cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
