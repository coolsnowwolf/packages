use super::*;
use serde_json::json;
use std::sync::{atomic::AtomicUsize, Arc};

#[test]
fn fpga_native_contract_rejects_write_fields_and_preserves_session_transport() {
    let (_, handler) = bind(&FPGA_DIAGNOSTIC, |request| {
        assert_eq!(request.slots().unwrap(), vec![15]);
        Ok(FpgaDiagnosticReply::Error {
            error: "fixture read unavailable".into(),
        })
    });
    assert_eq!(
        handler("fpga_diagnostic", Some(&json!({"onu_id":15,"write":1}))),
        Err(2)
    );
    assert_eq!(
        handler(
            "fpga_diagnostic",
            Some(&json!({"onu_id":15,"ubus_rpc_session":"REDACTED_TEST_SESSION"}))
        ),
        Ok(json!({"status":"error","error":"fixture read unavailable"}))
    );
}

#[test]
fn http_session_metadata_is_not_a_topology_business_argument() {
    let (_, node) = bind(&GET_TOPOLOGY_NODE, |request| {
        assert_eq!(request.node_id, "controller");
        assert_eq!(request.snapshot_id.as_deref(), Some("test-snapshot"));
        Ok(TopologyNodeReply::Error {
            code: TopologyQueryError::SnapshotExpired,
        })
    });
    let (_, clients) = bind(&GET_TOPOLOGY_CLIENTS, |request| {
        assert_eq!(request.node_id, "controller");
        assert_eq!(request.snapshot_id.as_deref(), Some("test-snapshot"));
        assert_eq!(request.limit, Some(100));
        assert_eq!(request.cursor, None);
        Ok(TopologyClientsReply::Error {
            code: TopologyQueryError::SnapshotExpired,
        })
    });
    for (name, handler, mut payload) in [
        (
            "get_topology_node",
            node,
            json!({"node_id":"controller", "snapshot_id":"test-snapshot"}),
        ),
        (
            "get_topology_clients",
            clients,
            json!({"node_id":"controller", "snapshot_id":"test-snapshot", "limit":100}),
        ),
    ] {
        let expected = handler(name, Some(&payload));
        assert_eq!(
            expected,
            Ok(json!({"status":"error", "code":"snapshot_expired"}))
        );
        payload["ubus_rpc_session"] = json!("REDACTED_TEST_SESSION");
        assert_eq!(handler(name, Some(&payload)), expected);
        payload["unexpected_argument"] = json!(true);
        assert_eq!(handler(name, Some(&payload)), Err(2));
    }
}

#[test]
fn invalid_native_write_requests_never_reach_the_handler() {
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let (name, handler) = bind(&NPU_SET, move |request| {
        invoked.fetch_add(1, Ordering::SeqCst);
        Ok(NpuSetReply::Success {
            enabled: request.enabled,
        })
    });
    for invalid in [
        None,
        Some(json!(null)),
        Some(json!([])),
        Some(json!({"enabled": "false", "bridge": "br-test"})),
        Some(json!({"enabled": false})),
    ] {
        assert_eq!(handler(&name, invalid.as_ref()), Err(2));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        handler(
            &name,
            Some(
                &json!({"enabled": false, "bridge": "br-test", "wan_upstream": false, "wan_downstream": false, "onu_bridge": true})
            )
        ),
        Ok(json!({"status": "success", "enabled": false}))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn native_dispatch_preserves_ubus_and_business_errors_without_retrying() {
    let calls = AtomicUsize::new(0);
    let (_, handler) = bind(&NPU_SET, move |_| {
        // Each invocation is handled once even if it fails.
        assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
        Err(4)
    });
    let request = json!({"enabled": true, "bridge": "br-test", "wan_upstream": false, "wan_downstream": false, "onu_bridge": true});
    assert_eq!(handler("npu_set", Some(&request)), Err(4));

    let (_, handler) = bind(&NPU_SET, |_| {
        Ok(NpuSetReply::Error {
            error: "adapter unavailable".into(),
        })
    });
    assert_eq!(
        handler("npu_set", Some(&request)),
        Ok(json!({"status": "error", "error": "adapter unavailable"}))
    );
}

#[cfg(feature = "device-hl202")]
#[test]
fn async_hl_results_follow_the_request_and_preserve_failure_evidence() {
    let target = HlOpticalSelection {
        optical_sn: "REDACTED".into(),
        onu_id: 0,
        binding_generation: "test-epoch".into(),
    };
    let query = |action| HlQuery::Run {
        request: HlCommandRequest {
            schema_version: 1,
            target: target.clone(),
            action,
        },
    };
    let exec = query(HlAction::Exec {
        script: "true".into(),
        timeout_seconds: 1,
    });
    let operation = json!({
        "target": {
            "mqtt_mac": "REDACTED", "optical_sn": null, "onu_id": null,
            "binding_generation": "test-epoch", "last_seen": 0, "reason": "test"
        },
        "result": {
            "target": "REDACTED", "status": "timeout", "output_truncated": true,
            "output": "partial output", "error": "timed out"
        },
        "binding_current": false
    });
    let result = json!({"schema_version": 1, "operation": operation});
    assert_eq!(
        normalize_hl_result(&exec, result.clone()),
        Ok(result.clone())
    );
    for action in [
        HlAction::AccountsRead,
        HlAction::AccountSetPassword {
            entry: 0,
            password: String::new(),
        },
        HlAction::AccountSetActive {
            entry: 1,
            active: false,
        },
    ] {
        let accounts = query(action);
        assert_eq!(
            normalize_hl_result(&accounts, result.clone()),
            Err(UBUS_ERROR_INTERNAL)
        );
        let mut payload = result.clone();
        payload["accounts"] = json!(null);
        payload["write_status"] = json!("unconfirmed");
        assert_eq!(normalize_hl_result(&accounts, payload.clone()), Ok(payload));
    }
    let report = HlQuery::Report { target };
    assert_eq!(
        normalize_hl_result(&report, result),
        Err(UBUS_ERROR_INTERNAL)
    );
    let error =
        json!({"schema_version": 1, "error": {"code": "unavailable", "message": "offline"}});
    for request in [&exec, &report, &HlQuery::List] {
        assert_eq!(
            normalize_hl_result(request, error.clone()),
            Ok(error.clone())
        );
    }
}
