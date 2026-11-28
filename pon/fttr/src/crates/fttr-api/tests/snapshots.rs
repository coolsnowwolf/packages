use fttr_api::*;
use fttr_omci::{
    mib::{MibAssembler, MibRecord, TableSnapshot},
    schema::MeRegistry,
};
use serde_json::{json, Value};

#[test]
fn success_envelope_versions_match_generated_literal_contracts() {
    let mut optical = json!({
        "schema_version": 1, "status": "success", "action": "optical_status",
        "desired_enabled": null, "tx_enabled": null, "actor_state": "unknown",
        "hardware_ready": false, "discovery_active": false, "cleanup_pending": false,
        "error": null
    });
    assert!(serde_json::from_value::<OpticalReply>(optical.clone()).is_ok());
    let mut list = json!({"schema_version": 1, "devices": [], "sources": []});
    assert!(serde_json::from_value::<HlListReply>(list.clone()).is_ok());
    for version in [0, 2, 255] {
        optical["schema_version"] = json!(version);
        list["schema_version"] = json!(version);
        assert!(serde_json::from_value::<OpticalReply>(optical.clone()).is_err());
        assert!(serde_json::from_value::<HlListReply>(list.clone()).is_err());
    }
}

#[test]
fn mib_producer_preserves_decoded_unknown_and_table_evidence() {
    let mut assembler = MibAssembler::new(2);
    assembler.push(MibRecord {
        sequence: 0,
        class_id: 256,
        entity_id: 0,
        attribute_mask: 0x8000,
        data: [0; 26],
    });
    assembler.push(MibRecord {
        sequence: 1,
        class_id: 65534,
        entity_id: 1,
        attribute_mask: 0x8000,
        data: [0xab; 26],
    });
    let mut report = assembler.finish();
    let attribute = MeRegistry::get(171)
        .unwrap()
        .attributes
        .iter()
        .find(|a| a.index == 6)
        .unwrap();
    report.tables.push(TableSnapshot::from_result(
        171,
        1,
        attribute,
        Ok(vec![0; 16]),
    ));
    let wire = serde_json::to_value(report).unwrap();
    let shared: MibReport = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    let mut malformed = wire;
    malformed["schema_version"] = json!(2);
    assert!(serde_json::from_value::<MibReport>(malformed).is_err());
}

#[test]
fn telemetry_producer_preserves_open_values_inside_typed_envelopes() {
    let report = HlTelemetry {
        schema_version: 1,
        compressed_bytes: 12,
        decompressed_bytes: 24,
        fields: [(
            "extension".into(),
            HlTelemetryField {
                value: json!({"vendor": [false, null, 3]}),
                member: "test.json".into(),
                unit: None,
            },
        )]
        .into(),
        members: vec![HlTelemetryMember {
            name: "test.json".into(),
            bytes: 24,
            encoding: "utf8".into(),
            text: None,
            text_omitted: true,
        }],
        errors: vec![],
    };
    let wire = serde_json::to_value(report).unwrap();
    let shared: HlTelemetry = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    let mut malformed = wire;
    malformed["members"][0]["text_omitted"] = json!("true");
    assert!(serde_json::from_value::<HlTelemetry>(malformed).is_err());
}

#[test]
fn provisioning_preserves_optional_observations_and_omci_results() {
    let unavailable = json!({"state":"pending","reason":"observation not started"});
    assert!(serde_json::from_value::<AcsBootstrap>(unavailable).is_ok());
    let omci = json!({"state":"association_verified","enabled":true,"reason":null,"revision":"test_revision","vlan":46,"updated_at":12});
    let shared: DeviceOmciManagement = serde_json::from_value(omci.clone()).unwrap();
    let normalized = serde_json::to_value(shared).unwrap();
    for key in ["state", "enabled", "revision", "vlan", "updated_at"] {
        assert_eq!(normalized[key], omci[key]);
    }
    let pending = json!({"state":"not_started","items":[]});
    let shared: ProvisioningStatus = serde_json::from_value(pending.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), pending);
    assert!(
        serde_json::from_value::<ProvisioningStatus>(json!({"state":"typo","items":[]})).is_err()
    );
    assert!(serde_json::from_value::<ProvisioningStatus>(
        json!({"state":"not_started","items":[{"id":"test"}]})
    )
    .is_err());
}

#[test]
fn asynchronous_result_contracts_keep_sparse_command_failures_and_literal_guards() {
    let target = json!({"mqtt_mac":"REDACTED","optical_sn":null,"onu_id":null,"binding_generation":"test","last_seen":0,"reason":"test"});
    let wire = json!({"schema_version":1,"operation":{"target":target,"result":{"target":"REDACTED","status":"timeout","output_truncated":false,"error":"outcome unknown"},"binding_current":false},"accounts":null,"write_status":"unconfirmed"});
    let shared: HlAccountReply = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    assert!(serde_json::from_value::<HlAccount>(
        json!({"entry":3,"username":"test","active":false})
    )
    .is_err());
    assert!(serde_json::from_value::<RestartReply>(json!({"restarting":false})).is_err());
    let source = json!({"schema_version":1,"target":target,"report":null,"read_only":true});
    assert!(serde_json::from_value::<HlReportReply>(source.clone()).is_ok());
    let mut bad: Value = source;
    bad["read_only"] = json!(false);
    assert!(serde_json::from_value::<HlReportReply>(bad).is_err());

    let bridge_without_rules = json!({
        "enabled": true,
        "state": "waiting",
        "lan_origin": "upstream",
        "active_rules": 0
    });
    let parsed: MqttBridgeStatus = serde_json::from_value(bridge_without_rules).unwrap();
    assert!(parsed.rules.is_empty());
    assert_eq!(serde_json::to_value(&parsed).unwrap()["rules"], json!([]));

    let bridge_with_rules = MqttBridgeStatus {
        enabled: true,
        state: "active".into(),
        lan_origin: "upstream".into(),
        broker_listen: Some("192.0.2.44:1883".into()),
        upstream_gateway: Some("192.0.2.1".into()),
        active_rules: 1,
        rules: vec![MqttBridgeRule {
            interface: "fttr127".into(),
            client_mac: "02:00:00:00:00:01".into(),
            client_ip: "192.0.2.244".into(),
            target_gw: "192.0.2.1".into(),
            broker_ip: "192.0.2.44".into(),
            bridge_mac: "02:00:00:00:00:02".into(),
            device: Some("onu_1".into()),
            optical_sn: Some("H3CT00000001".into()),
            state: Some("verified".into()),
            binding_source: Some("verified_report".into()),
            evidence_source: Some("dhcp_ack".into()),
            bridge_rule: "iifname \"fttr127\" ether saddr 02:00:00:00:00:01 ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 meta pkttype set host ether daddr set 02:00:00:00:00:02 counter".into(),
            ip_rule: "ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 counter dnat to 192.0.2.44:1883".into(),
        }],
        reason: None,
        error: None,
    };
    let wire = serde_json::to_value(&bridge_with_rules).unwrap();
    assert_eq!(
        serde_json::from_value::<MqttBridgeStatus>(wire).unwrap(),
        bridge_with_rules
    );

    let acs_bridge = AcsBridgeStatus {
        enabled: true,
        management_gateway: true,
        state: "active".into(),
        lan_origin: "upstream".into(),
        lan_listen: Some("192.0.2.44:7547".into()),
        management_listen: Some("192.0.2.1:7547".into()),
        path_mode: "wildcard".into(),
        accepted_paths: vec!["/cwmp".into(), "/tr069".into()],
        active_rules: 1,
        rules: vec![AcsBridgeRule {
            id: "rule_1".into(),
            name: Some("Default 10/8".into()),
            scope: "lan".into(),
            target_ip: "10.0.0.0/8".into(),
            target_port: 7547,
            path: None,
            path_mode: "wildcard".into(),
            device: Some("onu_1".into()),
            optical_sn: Some("H3CT00000001".into()),
            interfaces: vec!["fttr1".into()],
            acs_target: Some("192.0.2.44:7547".into()),
            bridge_rules: vec!["iifname \"fttr1\" ip daddr 10.0.0.0/8 tcp dport 7547 meta pkttype set host ether daddr set 02:00:00:00:00:02 counter".into()],
            ip_rules: vec!["ip daddr 10.0.0.0/8 tcp dport 7547 counter dnat to 192.0.2.44:7547".into()],
            state: "active".into(),
            reason: None,
        }],
        reason: None,
        error: None,
    };
    let acs_wire = serde_json::to_value(&acs_bridge).unwrap();
    assert_eq!(
        serde_json::from_value::<AcsBridgeStatus>(acs_wire).unwrap(),
        acs_bridge
    );
}
