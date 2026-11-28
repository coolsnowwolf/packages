use crate::model::{
    DataPathOffloadStatus, DataPathStatus, MeasurementEvidence, Measurements, OnuOmciInventory,
};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct Counters {
    at: Instant,
    tx: Option<u64>,
    rx: Option<u64>,
}

pub(super) fn data_path(
    id: u8,
    registration: &str,
    inventory: Option<OnuOmciInventory>,
) -> DataPathStatus {
    let mut status = data_path_from(Path::new("/sys/class/net"), id, registration, inventory);
    collect_offload(
        &mut status,
        id,
        Path::new("/sys/module/h3c_fmcs/parameters/routed_offload_mask"),
        Path::new("/sys/module/h3c_fmcs/parameters/bridge_offload_mask"),
        Path::new("/sys/kernel/debug/ppe/config"),
    );
    status
}

fn data_path_from(
    root: &Path,
    id: u8,
    registration: &str,
    inventory: Option<OnuOmciInventory>,
) -> DataPathStatus {
    let mut status = collect(root, id, registration);
    let expected_gem = 0x100u16 + u16::from(id);
    status.olt_channel_gem = Some(expected_gem);

    if let Some(inventory) = inventory {
        status.gem_ports = inventory
            .gem_ports
            .iter()
            .map(|port| port.port_id)
            .collect();
        for field in ["gem_ports", "onu_inventory"] {
            let mut evidence = MeasurementEvidence::observed(
                "fresh ONU OMCI MIB upload; not OLT forwarding proof",
                "ONU configuration",
                format!(
                    "{} GEM CTPs; {} VLAN filters; {} VLAN operation tables",
                    inventory.gem_ports.len(),
                    inventory.vlan_filters.len(),
                    inventory.vlan_operations.len()
                ),
            );
            evidence.age_seconds = Some(inventory.age_seconds);
            if field == "gem_ports" && status.gem_ports.is_none() {
                evidence.reason = Some("gem_port_attribute_missing_or_invalid".into());
            }
            status.measurements.insert(field.into(), evidence);
        }

        let gem_matched = status
            .gem_ports
            .as_ref()
            .map_or(false, |ports| ports.iter().any(|p| *p == expected_gem));
        let rx_traffic = status.rx_bytes.unwrap_or(0) > 0;

        let (verified, fwd_evidence) = if !gem_matched {
            (
                false,
                MeasurementEvidence::unavailable(
                    format!("OLT SerDes priority-0 channel 0x{expected_gem:04x}"),
                    "forwarding_proof",
                    format!("gem_port_mismatch_with_olt_hardware_envelope_expected_0x{expected_gem:04x}"),
                ),
            )
        } else if rx_traffic {
            (
                true,
                MeasurementEvidence::observed(
                    format!("{}/statistics", status.interface),
                    "forwarding_proof",
                    format!(
                        "gem_rx_observed_not_ppe_proof: rx_bytes={}, tx_bytes={}, gem_port=0x{:04x}",
                        status.rx_bytes.unwrap_or(0),
                        status.tx_bytes.unwrap_or(0),
                        expected_gem
                    ),
                ),
            )
        } else {
            (
                false,
                MeasurementEvidence::unavailable(
                    format!("{}/statistics", status.interface),
                    "forwarding_proof",
                    "waiting_for_first_rx_frame_from_gem",
                ),
            )
        };

        status.forwarding_verified = Some(verified);
        status
            .measurements
            .insert("forwarding_proof".into(), fwd_evidence);
        status.onu_inventory = Some(inventory);
    } else {
        status.forwarding_verified = Some(false);
        status.measurements.insert(
            "forwarding_proof".into(),
            MeasurementEvidence::unavailable(
                "verified data-path inventory",
                "forwarding_proof",
                "verified_inventory_unavailable",
            ),
        );
    }
    status
}

fn counter(path: &Path) -> (Option<u64>, MeasurementEvidence) {
    let source = path.display().to_string();
    let unit = if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("packets"))
    {
        "packets"
    } else {
        "bytes"
    };
    match fs::read_to_string(path) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(value) => (
                Some(value),
                MeasurementEvidence::observed(source, unit, raw.trim()),
            ),
            Err(error) => (
                None,
                MeasurementEvidence::unavailable(source, unit, format!("invalid_counter: {error}")),
            ),
        },
        Err(error) => (
            None,
            MeasurementEvidence::unavailable(source, unit, format!("read_failed: {error}")),
        ),
    }
}

fn collect_offload(
    status: &mut DataPathStatus,
    id: u8,
    mask_path: &Path,
    bridge_path: &Path,
    ppe_path: &Path,
) {
    let enabled = |path: &Path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u16>().ok())
            .and_then(|mask| (id < 16).then(|| mask & (1u16 << id) != 0))
    };
    let attached = fs::read_to_string(ppe_path).ok().and_then(|raw| {
        raw.lines().find_map(|line| match line.split_once(':') {
            Some(("npu_attached", value)) => match value.trim() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            },
            _ => None,
        })
    });
    status.offload.routed_enabled = enabled(mask_path);
    status.offload.bridge_enabled = enabled(bridge_path);
    status.offload.enabled = match (status.offload.routed_enabled, status.offload.bridge_enabled) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    status.offload.engine_attached = attached;
    for (field, value, path) in [
        (
            "offload_routed_enabled",
            status.offload.routed_enabled,
            mask_path,
        ),
        (
            "offload_bridge_enabled",
            status.offload.bridge_enabled,
            bridge_path,
        ),
        ("offload_engine_attached", attached, ppe_path),
    ] {
        let evidence = match value {
            Some(value) => MeasurementEvidence::observed(
                path.display().to_string(),
                "boolean",
                value.to_string(),
            ),
            None => MeasurementEvidence::unavailable(
                path.display().to_string(),
                "boolean",
                "unreadable_or_invalid_offload_state",
            ),
        };
        status.measurements.insert(field.into(), evidence);
    }
    status.offload.forwarding_verified = None;
    status.measurements.insert(
        "offload_forwarding_proof".into(),
        MeasurementEvidence::unavailable(
            "per-flow PPE hardware hit evidence",
            "forwarding_proof",
            "not_verified; policy_and_global_engine_state_do_not_prove_per_onu_hardware_forwarding",
        ),
    );
}

fn rate(current: Option<u64>, previous: Option<u64>, elapsed: Duration) -> Option<f64> {
    if elapsed < Duration::from_secs(1) {
        return None;
    }
    let delta = current?.checked_sub(previous?)?;
    Some(delta.to_string().parse::<f64>().ok()? * 8.0 / elapsed.as_secs_f64() / 1000.0)
}

fn collect(root: &Path, id: u8, registration: &str) -> DataPathStatus {
    static SAMPLES: OnceLock<Mutex<BTreeMap<String, Counters>>> = OnceLock::new();
    let interface = super::RESOURCES
        .interface(id)
        .unwrap_or("unmapped")
        .to_owned();
    let base = root.join(&interface).join("statistics");
    let mut measurements = Measurements::new();
    let (tx_bytes, tx) = counter(&base.join("tx_bytes"));
    let (rx_bytes, rx) = counter(&base.join("rx_bytes"));
    let (_tx_packets, tx_pkts) = counter(&base.join("tx_packets"));
    let (_rx_packets, rx_pkts) = counter(&base.join("rx_packets"));
    measurements.insert("tx_bytes".into(), tx);
    measurements.insert("rx_bytes".into(), rx);
    measurements.insert("tx_packets".into(), tx_pkts);
    measurements.insert("rx_packets".into(), rx_pkts);
    let mut current_tx_kbps = None;
    let mut current_rx_kbps = None;
    if let Ok(mut samples) = SAMPLES.get_or_init(|| Mutex::new(BTreeMap::new())).lock() {
        let key = format!("{}:{interface}:{registration}", root.display());
        let previous = samples.get(&key);
        if let Some(previous) = previous {
            current_tx_kbps = rate(tx_bytes, previous.tx, previous.at.elapsed());
            current_rx_kbps = rate(rx_bytes, previous.rx, previous.at.elapsed());
        }
        if previous.is_none_or(|sample| sample.at.elapsed() >= Duration::from_secs(1)) {
            samples.insert(
                key,
                Counters {
                    at: Instant::now(),
                    tx: tx_bytes,
                    rx: rx_bytes,
                },
            );
        }
    }
    for (name, value) in [
        ("current_tx_kbps", current_tx_kbps),
        ("current_rx_kbps", current_rx_kbps),
    ] {
        let evidence = match value {
            Some(value) => MeasurementEvidence::observed(
                format!("{}/counter_delta", base.display()),
                "kbit/s",
                value.to_string(),
            ),
            None => MeasurementEvidence::unavailable(
                format!("{}/counter_delta", base.display()),
                "kbit/s",
                "requires_two_valid_monotonic_samples_at_least_one_second_apart",
            ),
        };
        measurements.insert(name.into(), evidence);
    }
    for field in ["gem_ports", "onu_inventory"] {
        measurements.insert(
            field.into(),
            MeasurementEvidence::unavailable(
                "verified data-path inventory",
                "identifier",
                "verified_inventory_unavailable",
            ),
        );
    }
    measurements.insert(
        "vlan_id".into(),
        MeasurementEvidence::unavailable(
            "ONU VLAN rule tables",
            "VID",
            "vlan_rules_are_not_a_single_business_vlan; see onu_inventory",
        ),
    );
    DataPathStatus {
        interface,
        gem_ports: None,
        vlan_id: None,
        onu_inventory: None,
        tx_bytes,
        rx_bytes,
        current_tx_kbps,
        current_rx_kbps,
        olt_channel_gem: None,
        forwarding_verified: None,
        offload: DataPathOffloadStatus::default(),
        measurements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_traffic_when_counters_unchanged() {
        let previous = Some(42);
        let actual = rate(Some(42), previous, Duration::from_secs(2));
        assert_eq!(actual, Some(0.0));
    }

    #[test]
    fn unavailable_when_counter_reset() {
        let previous = Some(42);
        let actual = rate(Some(2), previous, Duration::from_secs(2));
        assert_eq!(actual, None);
    }

    #[test]
    fn converts_bytes_to_decimal_kilobits_when_sampled() {
        let elapsed = Duration::from_secs(2);
        let actual = rate(Some(1500), Some(500), elapsed);
        assert_eq!(actual, Some(4.0));
    }

    #[test]
    fn data_path_observes_gem_rx_without_claiming_ppe_forwarding() {
        let root = std::env::temp_dir().join(format!("network-test-ok-{}", std::process::id()));
        let stat_dir = root.join("fttr2").join("statistics");
        fs::create_dir_all(&stat_dir).unwrap();
        fs::write(stat_dir.join("tx_bytes"), "1000\n").unwrap();
        fs::write(stat_dir.join("rx_bytes"), "2000\n").unwrap();
        fs::write(stat_dir.join("tx_packets"), "10\n").unwrap();
        fs::write(stat_dir.join("rx_packets"), "20\n").unwrap();

        let inventory = OnuOmciInventory {
            age_seconds: 5,
            gem_ports: vec![crate::model::OnuGemPort {
                entity_id: 257,
                port_id: Some(257),
            }],
            vlan_filters: Vec::new(),
            vlan_operations: Vec::new(),
        };

        let status = data_path_from(&root, 1, "test_sn", Some(inventory));
        assert_eq!(status.olt_channel_gem, Some(257));
        assert_eq!(status.forwarding_verified, Some(true));
        let proof = status.measurements.get("forwarding_proof").unwrap();
        assert!(proof.raw.is_some());
        assert!(proof.reason.is_none());
        assert_eq!(proof.unit, "forwarding_proof");
        assert!(proof
            .raw
            .as_ref()
            .unwrap()
            .contains("gem_rx_observed_not_ppe_proof"));
        assert_eq!(status.offload.forwarding_verified, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn data_path_withholds_verification_when_no_rx_traffic_received() {
        let root = std::env::temp_dir().join(format!("network-test-norx-{}", std::process::id()));
        let stat_dir = root.join("fttr2").join("statistics");
        fs::create_dir_all(&stat_dir).unwrap();
        fs::write(stat_dir.join("tx_bytes"), "500\n").unwrap();
        fs::write(stat_dir.join("rx_bytes"), "0\n").unwrap();

        let inventory = OnuOmciInventory {
            age_seconds: 5,
            gem_ports: vec![crate::model::OnuGemPort {
                entity_id: 257,
                port_id: Some(257),
            }],
            vlan_filters: Vec::new(),
            vlan_operations: Vec::new(),
        };

        let status = data_path_from(&root, 1, "test_sn", Some(inventory));
        assert_eq!(status.olt_channel_gem, Some(257));
        assert_eq!(status.forwarding_verified, Some(false));
        let proof = status.measurements.get("forwarding_proof").unwrap();
        assert!(proof.raw.is_none());
        assert_eq!(
            proof.reason.as_deref(),
            Some("waiting_for_first_rx_frame_from_gem")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn data_path_rejects_verification_when_gem_mismatches_olt_channel() {
        let root =
            std::env::temp_dir().join(format!("network-test-mismatch-{}", std::process::id()));
        let stat_dir = root.join("fttr2").join("statistics");
        fs::create_dir_all(&stat_dir).unwrap();
        fs::write(stat_dir.join("tx_bytes"), "1000\n").unwrap();
        fs::write(stat_dir.join("rx_bytes"), "2000\n").unwrap();

        // ONU reports GEM 300, but OLT channel for ONU 1 is 257 (0x101)
        let inventory = OnuOmciInventory {
            age_seconds: 5,
            gem_ports: vec![crate::model::OnuGemPort {
                entity_id: 257,
                port_id: Some(300),
            }],
            vlan_filters: Vec::new(),
            vlan_operations: Vec::new(),
        };

        let status = data_path_from(&root, 1, "test_sn", Some(inventory));
        assert_eq!(status.olt_channel_gem, Some(257));
        assert_eq!(status.forwarding_verified, Some(false));
        let proof = status.measurements.get("forwarding_proof").unwrap();
        assert!(proof.raw.is_none());
        assert!(proof
            .reason
            .as_ref()
            .unwrap()
            .contains("gem_port_mismatch_with_olt_hardware_envelope"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn data_path_withholds_verification_without_verified_inventory() {
        let root = std::env::temp_dir().join(format!("network-test-noinv-{}", std::process::id()));
        let status = data_path_from(&root, 2, "test_sn", None);
        assert_eq!(status.olt_channel_gem, Some(258));
        assert_eq!(status.forwarding_verified, Some(false));
        let proof = status.measurements.get("forwarding_proof").unwrap();
        assert!(proof.raw.is_none());
        assert_eq!(
            proof.reason.as_deref(),
            Some("verified_inventory_unavailable")
        );
    }

    #[test]
    fn offload_policy_and_attachment_are_not_hardware_hit_proof() {
        let root = std::env::temp_dir().join(format!("network-offload-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mask = root.join("mask");
        let bridge = root.join("bridge");
        fs::write(&bridge, "0\n").unwrap();
        let ppe = root.join("ppe");
        let mut status = collect(&root, 1, "test");
        collect_offload(&mut status, 1, &mask, &bridge, &ppe);
        assert_eq!(status.offload.enabled, None);
        assert_eq!(status.offload.engine_attached, None);
        fs::write(&mask, "2\n").unwrap();
        fs::write(&ppe, "npu_attached: 1\nppe0_flow_cfg: ffffffff\n").unwrap();
        collect_offload(&mut status, 1, &mask, &bridge, &ppe);
        assert_eq!(status.offload.enabled, Some(true));
        assert_eq!(status.offload.engine_attached, Some(true));
        assert_eq!(status.offload.forwarding_verified, None);
        assert!(status.measurements["offload_forwarding_proof"]
            .reason
            .is_some());
        collect_offload(&mut status, 2, &mask, &bridge, &ppe);
        assert_eq!(status.offload.enabled, Some(false));
        fs::write(&bridge, "4\n").unwrap();
        collect_offload(&mut status, 2, &mask, &bridge, &ppe);
        assert_eq!(status.offload.routed_enabled, Some(false));
        assert_eq!(status.offload.bridge_enabled, Some(true));
        assert_eq!(status.offload.enabled, Some(true));
        collect_offload(&mut status, 16, &mask, &bridge, &ppe);
        assert_eq!(status.offload.enabled, None);
        fs::write(&mask, "65536\n").unwrap();
        fs::write(&ppe, "npu_attached: invalid\n").unwrap();
        collect_offload(&mut status, 1, &mask, &bridge, &ppe);
        assert_eq!(status.offload.enabled, None);
        assert_eq!(status.offload.engine_attached, None);
        fs::remove_dir_all(&root).unwrap();
    }
}
