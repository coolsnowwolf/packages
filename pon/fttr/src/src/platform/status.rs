use crate::{
    ipc::SubDevice,
    model::{
        FttrSystemStatus, MeasurementEvidence, SubGatewayStatus, SwitchPortStatus,
        TelemetryGatewayInfo, WifiMeshStatus,
    },
    pon_engine::{ploam::OnuState, PonEngine},
};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn snapshot(
    engine: &PonEngine,
    services: &crate::controller::service::States,
    sources: (&[SubDevice], fttr_api::SyncStatusReply),
    hardware: Hardware,
) -> FttrSystemStatus {
    let (devs, provisioning) = sources;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let switch_ports = (1..=hardware.switch_port_count)
        .map(|port| {
            let interface = format!("fttr{port}");
            let onu = engine.active_onus.iter().find(|onu| onu.onu_id == port - 1);
            let carrier = onu.is_some_and(|onu| onu.state == OnuState::O5Operation)
                && fs::read_to_string(format!("/sys/class/net/{interface}/carrier"))
                    .is_ok_and(|value| value.trim() == "1");
            let (onu_id, onu_sn, link_state) = match onu {
                Some(onu) => {
                    let state = match onu.state {
                        OnuState::O1Initial => "Offline (identity retained)",
                        OnuState::O2Standby => "O2Standby",
                        OnuState::O3SerialNumber => "O3SerialNumber",
                        OnuState::O4Ranging if onu.eq_delay.is_some() => "EqD sent; O5 unconfirmed",
                        OnuState::O4Ranging => "Awaiting ranging (O4)",
                        OnuState::O5Operation => "O5 (protocol confirmed)",
                        OnuState::O6IntermittentLODS => "O6IntermittentLODS",
                        OnuState::O7EmergencyStop => "O7EmergencyStop",
                    };
                    (Some(onu.onu_id), Some(onu.full_sn_str()), state.to_string())
                }
                None => (None, None, "Offline".into()),
            };
            SwitchPortStatus {
                port,
                interface,
                carrier,
                status: if carrier { "UP" } else { "DOWN" }.into(),
                onu_id,
                onu_sn,
                link_state,
                reserved_sn: engine
                    .onu_pins
                    .iter()
                    .find(|(_, id)| **id == port - 1)
                    .map(|(sn, _)| sn.clone()),
            }
        })
        .collect();
    let sub_gateways = engine
        .active_onus
        .iter()
        .map(|onu| {
            let service = services.lock().unwrap().get(&onu.onu_id).cloned();
            let identity = service.as_ref().and_then(|state| state.identity.clone());
            let sn_str = onu.full_sn_str();
            let desired = provisioning.devices.iter().find(|d| d.optical_sn == sn_str);
            let mut telemetry = crate::controller::telemetry::snapshot(
                onu.onu_id,
                onu.state == OnuState::O5Operation,
            );
            if !hardware.has_pon_mac_fec {
                telemetry.fec.downstream_enabled = None;
            }
            if let Some(&upstream_enabled) = hardware.onu_us_fec.get(&onu.onu_id) {
                telemetry.fec.upstream_enabled = Some(upstream_enabled);
            }
            for (field, unit, reason) in [
                (
                    "fiber_distance_m",
                    "m",
                    "calibrated_propagation_measurement_unavailable",
                ),
                (
                    "ranging_delay_rtd_ns",
                    "ns",
                    "equalization_delay_is_not_round_trip_propagation_delay",
                ),
                (
                    "connected_clients",
                    "stations",
                    "station_inventory_unavailable",
                ),
            ] {
                telemetry.measurements.insert(
                    field.into(),
                    MeasurementEvidence::unavailable("FTTR", unit, reason),
                );
            }
            let equalization_delay_bits = onu.eq_delay;
            telemetry.measurements.insert(
                "equalization_delay_bits".into(),
                match equalization_delay_bits {
                    Some(bits) => MeasurementEvidence::observed(
                        (hardware.ranging_source)(onu.onu_id),
                        "bits",
                        bits.to_string(),
                    ),
                    None => MeasurementEvidence::unavailable(
                        (hardware.ranging_source)(onu.onu_id),
                        "bits",
                        "ranging_measurement_unavailable",
                    ),
                },
            );
            SubGatewayStatus {
                onu_id: onu.onu_id,
                vendor: onu.vendor_str(),
                model: identity
                    .as_ref()
                    .and_then(|value| value.equipment_id.clone())
                    .unwrap_or_else(|| onu.model.clone()),
                serial_number: sn_str.clone(),
                state: onu.state,
                pon_mode: onu.pon_mode.clone(),
                upstream_phy_rate_gbps: onu.upstream_phy_rate_gbps,
                omci_auth: Some(crate::omci_auth::status(&sn_str)),
                management: desired.and_then(|d| d.management.clone()),
                provisioning: desired.map(|d| d.provisioning.clone()),
                services: desired.map(|d| d.services.clone()).unwrap_or_default(),
                ssids: desired.map(|d| d.ssids.clone()).unwrap_or_default(),
                wifi_sync: desired.map(|d| d.wifi_sync).unwrap_or_default(),
                business_status: services
                    .lock()
                    .unwrap()
                    .get(&onu.onu_id)
                    .map(|state| {
                        let verified = fs::read_to_string(format!(
                            "/sys/class/net/{}/carrier",
                            engine.resources.interface(onu.onu_id).unwrap_or("unmapped")
                        ))
                        .is_ok_and(|value| value.trim() == "1");
                        state.business_status(
                            verified,
                            onu.state == OnuState::O5Operation
                                && desired
                                    .and_then(|d| d.management.as_ref())
                                    .is_some_and(|m| m.state == fttr_api::ManagementState::Online),
                        )
                    })
                    .unwrap_or_default(),
                identity: identity.clone(),
                identity_age_seconds: service
                    .as_ref()
                    .and_then(|state| state.identity_at)
                    .map(|at| at.elapsed().as_secs()),
                identity_current: onu.state == OnuState::O5Operation && identity.is_some(),
                fiber_distance_m: None,
                ranging_delay_rtd_ns: None,
                equalization_delay_bits,
                optical_rx_power_dbm: telemetry.rx,
                optical_tx_power_dbm: telemetry.tx,
                firmware_version: identity
                    .as_ref()
                    .filter(|value| !value.software_images.is_empty())
                    .map(|value| value.software_images.join("; "))
                    .unwrap_or_else(|| onu.firmware_version.clone()),
                hardware_version: identity
                    .as_ref()
                    .and_then(|value| value.hardware_version.clone())
                    .unwrap_or_else(|| onu.hardware_version.clone()),
                uptime_seconds: telemetry.uptime,
                fec: Some(telemetry.fec),
                data_path: (hardware.data_path)(
                    onu.onu_id,
                    &sn_str,
                    telemetry
                        .inventory
                        .as_ref()
                        .and_then(|sample| sample.current()),
                ),
                wifi_mesh: WifiMeshStatus {
                    channel_2g: None,
                    channel_5g: None,
                    bandwidth_5g: None,
                    tx_power_pct: None,
                    measurements: [
                        ("channel_2g", "channel"),
                        ("channel_5g", "channel"),
                        ("bandwidth_5g", "MHz"),
                        ("tx_power_pct", "percent"),
                    ]
                    .into_iter()
                    .map(|(field, unit)| {
                        (
                            field.into(),
                            MeasurementEvidence::unavailable(
                                "ONU radio telemetry",
                                unit,
                                "verified_radio_report_unavailable",
                            ),
                        )
                    })
                    .collect(),
                },
                connected_clients: None,
                measurements: telemetry.measurements,
            }
        })
        .collect();
    let telemetry_gateways = devs
        .iter()
        .map(|dev| TelemetryGatewayInfo {
            id: dev.id.clone(),
            last_seen_sec_ago: now.saturating_sub(dev.last_seen),
            last_payload_bytes: dev.payload_bytes,
        })
        .collect();
    let onu_pinning = pinning_status(
        engine,
        provisioning.onu_pins.as_ref(),
        provisioning.config_error.clone(),
    );
    FttrSystemStatus {
        onu_pinning: hardware.supports_pinning.then_some(onu_pinning),
        provisioning: Some(provisioning),
        ubus: None,
        jobs: None,
        ipc: None,
        fpga_status: hardware.fpga_status,
        pon_mac: hardware.pon_mac,
        optical_transceiver: hardware.optical_transceiver,
        switch_ports,
        sub_gateways,
        telemetry_gateways,
        npu: hardware.npu,
    }
}

pub(crate) struct Hardware {
    pub switch_port_count: u8,
    pub supports_pinning: bool,
    pub ranging_source: fn(u8) -> String,
    pub has_pon_mac_fec: bool,
    pub onu_us_fec: std::collections::BTreeMap<u8, bool>,
    pub data_path: fn(u8, &str, Option<fttr_api::OnuOmciInventory>) -> fttr_api::DataPathStatus,
    pub fpga_status: String,
    pub pon_mac: Option<fttr_api::PonMacStatus>,
    pub optical_transceiver: fttr_api::OpticalTransceiverStatus,
    pub npu: Option<fttr_api::NpuStatus>,
}

fn pinning_status(
    engine: &PonEngine,
    desired: Option<&std::collections::BTreeMap<String, u8>>,
    config_error: Option<String>,
) -> fttr_api::OnuPinning {
    let desired = desired.cloned().unwrap_or_default();
    fttr_api::OnuPinning {
        pending_restart: engine.pin_error.is_some() || desired != engine.onu_pins,
        desired,
        active: engine.onu_pins.clone(),
        allocation_blocked: engine.pin_error.is_some(),
        startup_error: engine.pin_error.clone(),
        config_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_reload_reports_pending_without_reassigning_or_inventing_onus() {
        let mut engine = crate::pon_engine::test_engine();
        engine.onu_pins.insert("TEST00000001".into(), 0);
        let original = engine.onu_pins.clone();
        assert!(!pinning_status(&engine, Some(&original), None).pending_restart);
        for desired in [
            std::collections::BTreeMap::new(),
            [("TEST00000001".into(), 15)].into(),
            [("TEST00000002".into(), 0)].into(),
        ] {
            let status = pinning_status(&engine, Some(&desired), None);
            assert!(status.pending_restart);
            assert_eq!(status.active, original);
            assert!(engine.active_onus.is_empty());
        }
        // Reverting the desired configuration cancels the pending change.
        assert!(!pinning_status(&engine, Some(&original), None).pending_restart);
        let invalid = pinning_status(&engine, Some(&original), Some("duplicate slot".into()));
        assert_eq!(invalid.config_error.as_deref(), Some("duplicate slot"));
        assert_eq!(invalid.active, original);
        engine.pin_error = Some("unreadable UCI at startup".into());
        let failed = pinning_status(&engine, Some(&original), None);
        assert!(failed.allocation_blocked);
        assert!(failed.pending_restart);
    }
}
