use super::*;
use serde_json::json;

fn project(
    status: StatusReply,
    local: local::Local,
    extensions: BTreeMap<String, TopologyHl202>,
) -> Snapshot {
    super::project(status, local, extensions, unix_time())
}

/// Pipe a private JSON object containing `status` and `samples` into this test.
/// Never persist or print the input: it can contain device/network identities.
#[test]
#[ignore = "requires private read-only ubus samples on stdin"]
fn readonly_source_sample_projection() {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let sample: serde_json::Value =
        serde_json::from_str(&input).expect("invalid private sample JSON");
    let status: StatusReply =
        serde_json::from_value(sample["status"].clone()).expect("invalid status contract");
    let expected: Vec<_> = status
        .sub_gateways
        .iter()
        .map(|onu| {
            (
                onu.onu_id,
                onu.optical_rx_power_dbm,
                onu.optical_tx_power_dbm,
            )
        })
        .collect();
    let local = local::collect_with(|object, method| {
        sample["samples"].get(format!("{object}.{method}")).cloned()
    });
    assert!(local.model.is_some(), "board model missing");
    assert!(local.uptime.is_some(), "system uptime missing");
    assert!(!local.interfaces.is_empty(), "network inventory missing");
    assert!(
        local.interfaces.iter().any(|i| i.carrier.is_some()),
        "nested carrier not decoded"
    );
    let snapshot = project(status, local, BTreeMap::new());
    assert!(
        snapshot.reply.nodes.len() > expected.len(),
        "ONU identities lost"
    );
    for (id, rx, tx) in expected {
        let node = snapshot.reply.nodes.iter().find(|node| matches!(&node.detail, TopologyNodeDetail::Onu(onu) if onu.onu_id == Some(id))).expect("ONU missing");
        let link = snapshot
            .reply
            .links
            .iter()
            .find(|link| link.target == node.id)
            .expect("ONU link missing");
        assert!(
            link.onu_rx_power_dbm == rx && link.onu_tx_power_dbm == tx,
            "optical readings were changed"
        );
    }
    assert!(
        serde_json::to_value(&snapshot.reply).is_ok(),
        "topology cannot serialize"
    );
}

fn status(onus: Vec<SubGatewayStatus>) -> StatusReply {
    StatusReply {
        ubus: None,
        jobs: None,
        ipc: None,
        onu_pinning: None,
        provisioning: None,
        npu: None,
        fpga_status: "Ready".into(),
        pon_mac: None,
        switch_ports: Vec::new(),
        sub_gateways: onus,
        telemetry_gateways: Vec::new(),
        optical_transceiver: OpticalTransceiverStatus {
            calibration_sha256: None,
            calibration_applied: false,
            model: "test-module".into(),
            wavelength_tx_nm: 1490,
            wavelength_rx_nm: 1310,
            phy_rate_downlink_gbps: 2.488,
            phy_rate_uplink_gbps: 1.244,
            tx_power_dbm: Some(0.0),
            laser_bias_current_ma: None,
            temperature_celsius: None,
            vcc_voltage: None,
            tx_enabled: Some(true),
            measurements: BTreeMap::new(),
        },
    }
}

fn onu(sn: &str, id: u8) -> SubGatewayStatus {
    SubGatewayStatus {
        onu_id: id,
        vendor: "TEST".into(),
        model: "test-onu".into(),
        serial_number: sn.into(),
        state: RegistrationState::O5Operation,
        pon_mode: None,
        upstream_phy_rate_gbps: None,
        business_status: BusinessStatus::default(),
        omci_auth: None,
        management: None,
        provisioning: None,
        services: Vec::new(),
        ssids: Vec::new(),
        wifi_sync: WifiSync::default(),
        identity: None,
        identity_age_seconds: None,
        identity_current: false,
        fiber_distance_m: None,
        ranging_delay_rtd_ns: None,
        equalization_delay_bits: Some(0),
        optical_rx_power_dbm: Some(0.0),
        optical_tx_power_dbm: None,
        firmware_version: "Unknown".into(),
        hardware_version: String::new(),
        uptime_seconds: Some(0.0),
        fec: None,
        data_path: DataPathStatus {
            interface: format!("testport{id}"),
            gem_ports: None,
            vlan_id: None,
            onu_inventory: None,
            tx_bytes: Some(0),
            rx_bytes: Some(0),
            current_tx_kbps: Some(0.0),
            current_rx_kbps: None,
            olt_channel_gem: None,
            forwarding_verified: Some(false),
            offload: DataPathOffloadStatus::default(),
            measurements: BTreeMap::new(),
        },
        wifi_mesh: WifiMeshStatus {
            channel_2g: None,
            channel_5g: None,
            bandwidth_5g: None,
            tx_power_pct: None,
            measurements: BTreeMap::new(),
        },
        connected_clients: None,
        measurements: BTreeMap::new(),
    }
}

#[test]
fn shared_nni_is_not_an_onu_port_or_a_port_conflict() {
    let mut a = onu("TEST00000001", 0);
    let mut b = onu("TEST00000002", 1);
    for onu in [&mut a, &mut b] {
        onu.data_path.interface = "fttr0".into();
        onu.data_path.measurements.insert(
            "interface_scope".into(),
            MeasurementEvidence::observed("FSL platform", "scope", "shared_nni"),
        );
    }
    let snapshot = project(status(vec![a, b]), local::Local::default(), BTreeMap::new());
    assert!(!snapshot
        .reply
        .issues
        .iter()
        .any(|issue| issue.code == "port_conflict"));
    let links: Vec<_> = snapshot
        .reply
        .links
        .iter()
        .filter(|link| link.data_interface.as_deref() == Some("fttr0"))
        .collect();
    assert_eq!(links.len(), 2);
    assert!(links.iter().all(|link| link.carrier.is_none()));
}

#[test]
fn stable_identity_survives_slot_change_and_unknown_is_not_zero() {
    let first = project(
        status(vec![onu(" test00000001 ", 0)]),
        local::Local::default(),
        BTreeMap::new(),
    );
    let second = project(
        status(vec![onu("TEST00000001", 15)]),
        local::Local::default(),
        BTreeMap::new(),
    );
    assert_eq!(first.reply.nodes[1].id, second.reply.nodes[1].id);
    assert_eq!(
        second.reply.links[0].data_interface.as_deref(),
        Some("testport15")
    );
    let value = serde_json::to_value(&first.reply).unwrap();
    let detail = &value["nodes"][1]["detail"];
    assert_eq!(detail["uptime_seconds"], 0.0);
    assert_eq!(detail["firmware_version"], serde_json::Value::Null);
    assert_eq!(value["links"][0]["onu_rx_power_dbm"], 0.0);
    assert_eq!(value["links"][0]["data_path"]["rx_bytes"], 0);
    for forbidden in [
        "fiber_distance_m",
        "ranging_delay_rtd_ns",
        "cdr_locked",
        "connected_clients",
        "channel_2g",
        "rssi_dbm",
        "vlan_id",
    ] {
        assert!(
            !value.to_string().contains(forbidden),
            "unimplemented field {forbidden}"
        );
    }
}

#[test]
fn only_real_pin_identities_create_placeholders_and_ids_are_not_port_numbers() {
    let mut state = status(Vec::new());
    state.onu_pinning = Some(OnuPinning {
        active: [("TEST00000001".into(), 0)].into(),
        desired: [("TEST00000001".into(), 15)].into(),
        pending_restart: true,
        allocation_blocked: false,
        startup_error: None,
        config_error: None,
    });
    let snapshot = project(state, local::Local::default(), BTreeMap::new());
    assert_eq!(snapshot.reply.nodes.len(), 2);
    let TopologyNodeDetail::Onu(onu) = &snapshot.reply.nodes[1].detail else {
        panic!()
    };
    assert_eq!(onu.registration, None);
    assert_eq!(onu.onu_id, None);
    assert_eq!(onu.binding.active_onu_id, Some(0));
    assert_eq!(onu.binding.desired_onu_id, Some(15));
    assert_eq!(onu.binding.state, TopologyBindingState::PendingChange);
    assert!(snapshot.reply.links[0].data_interface.is_none());
    assert!(snapshot.reply.links[0].carrier.is_none());
}

#[test]
fn duplicate_and_missing_serials_are_explicit_and_not_silently_merged() {
    let snapshot = project(
        status(vec![
            onu("TEST00000001", 0),
            onu("test00000001", 1),
            onu("", 2),
        ]),
        local::Local::default(),
        BTreeMap::new(),
    );
    assert_eq!(snapshot.reply.nodes.len(), 4);
    assert_eq!(
        snapshot
            .reply
            .issues
            .iter()
            .filter(|i| i.code == "duplicate_serial")
            .count(),
        2
    );
    assert!(snapshot
        .reply
        .issues
        .iter()
        .any(|i| i.code == "missing_serial"));
    assert_ne!(snapshot.reply.nodes[1].id, snapshot.reply.nodes[2].id);
}

#[test]
fn nested_carrier_unknown_speeds_and_route_presence_are_normalized() {
    let interfaces = local::interfaces(&json!({
        "lan1": {"devtype":"ethernet", "link":{"carrier":true,"speed":2500,"duplex":"full"}},
        "br-test": {"devtype":"bridge", "link":{"carrier":false,"speed":-1,"duplex":"unknown"}},
        "fttr1": {"devtype":"ethernet", "link":{"carrier":true,"speed":10000}},
        "missing": {}
    }));
    let get = |name| interfaces.iter().find(|i| i.name == name).unwrap();
    assert_eq!(get("lan1").carrier, Some(true));
    assert_eq!(get("lan1").speed_mbps, Some(2500));
    assert_eq!(get("br-test").carrier, Some(false));
    assert_eq!(get("br-test").speed_mbps, None);
    assert_eq!(get("fttr1").speed_mbps, None);
    assert_eq!(get("missing").carrier, None);
    let uplinks = local::uplinks(&json!({"interface":[
        {"interface":"test-wan","up":true,"proto":"dhcp","route":[{"target":"0.0.0.0","mask":0}]},
        {"interface":"test-lan","up":true,"route":[{"target":"0.0.0.0","mask":24}]}
    ]}));
    assert_eq!(uplinks.len(), 1);
    assert_eq!(uplinks[0].interface, "test-wan");
}

#[test]
fn cache_is_bounded_expires_and_detail_uses_the_exact_snapshot() {
    let mut cache = Cache::default();
    let first = cache.insert(project(
        status(Vec::new()),
        local::Local::default(),
        BTreeMap::new(),
    ));
    assert!(matches!(
        first.node("controller"),
        TopologyNodeReply::Ok { .. }
    ));
    assert!(matches!(
        first.node("absent"),
        TopologyNodeReply::Error {
            code: TopologyQueryError::NodeNotFound
        }
    ));
    for _ in 0..CACHE_SIZE {
        cache.insert(project(
            status(Vec::new()),
            local::Local::default(),
            BTreeMap::new(),
        ));
    }
    assert_eq!(cache.0.len(), CACHE_SIZE);
    assert!(cache.get(&first.reply.snapshot_id).is_none());
    let mut expired = project(status(Vec::new()), local::Local::default(), BTreeMap::new());
    expired.at = Instant::now() - Duration::from_secs(u64::from(TTL) + 1);
    let id = expired.reply.snapshot_id.clone();
    cache.insert(expired);
    assert!(cache.get(&id).is_none());
}

#[test]
fn pages_are_snapshot_bound_and_unknown_inventory_is_not_an_empty_success_count() {
    let mut snapshot = project(
        status(vec![onu("TEST00000001", 0)]),
        local::Local::default(),
        BTreeMap::new(),
    );
    let parent = snapshot.reply.nodes[1].id.clone();
    snapshot.clients.insert(
        parent.clone(),
        (0..3)
            .map(|i| TopologyClient {
                id: format!("host:{i}"),
                mac: format!("02:00:00:00:00:{i:02X}"),
                hostname: None,
                addresses: Vec::new(),
                relation: TopologyRelation::LearnedBehind,
                source: "bridge.fdb".into(),
                interface: None,
            })
            .collect(),
    );
    let mut request = TopologyClientsRequest {
        node_id: parent,
        snapshot_id: Some(snapshot.reply.snapshot_id.clone()),
        cursor: None,
        limit: Some(2),
    };
    let TopologyClientsReply::Ok {
        clients,
        next_cursor,
        learned_host_count,
        associated_client_count,
        complete,
        ..
    } = snapshot.clients(&request)
    else {
        panic!()
    };
    assert_eq!(clients.len(), 2);
    assert!(learned_host_count.is_none());
    assert!(associated_client_count.is_none());
    assert!(!complete);
    request.cursor = next_cursor;
    let TopologyClientsReply::Ok {
        clients,
        next_cursor,
        ..
    } = snapshot.clients(&request)
    else {
        panic!()
    };
    assert_eq!(clients.len(), 1);
    assert!(next_cursor.is_none());
    request.snapshot_id = None;
    assert!(matches!(
        snapshot.clients(&request),
        TopologyClientsReply::Error {
            code: TopologyQueryError::InvalidCursor
        }
    ));
}

#[cfg(feature = "device-hl202")]
#[test]
fn hl202_projection_whitelists_fields_and_rejects_stale_or_failed_reports() {
    let mut state = HlReportState {
        revision: 1,
        last_received: 100,
        last_processed_receipt: Some(100),
        last_good: Some(100),
        error: None,
        report: Some(HlTelemetry {
            schema_version: 1,
            compressed_bytes: 0,
            decompressed_bytes: 0,
            errors: Vec::new(),
            members: vec![HlTelemetryMember {
                name: "private-config".into(),
                bytes: 1,
                encoding: "text".into(),
                text: Some("secret-must-not-leak".into()),
                text_omitted: false,
            }],
            fields: [(
                "cpu_usage".into(),
                HlTelemetryField {
                    value: json!(0),
                    member: "Info_SysDevice.CpuUsage".into(),
                    unit: None,
                },
            )]
            .into(),
        }),
    };
    let ext = hl202::project(state.clone(), "test-generation".into(), 101).unwrap();
    assert_eq!(ext.cpu_usage_raw, Some(0));
    assert_eq!(ext.memory_usage_raw, None);
    assert!(!serde_json::to_string(&ext).unwrap().contains("secret"));
    assert!(hl202::project(state.clone(), "test-generation".into(), 190).is_none());
    assert!(hl202::project(state.clone(), "test-generation".into(), 99).is_none());
    state.error = Some("decode_failed".into());
    assert!(hl202::project(state, "test-generation".into(), 101).is_none());
}
