use fttr_api::*;
use serde_json::json;

#[test]
fn operation_parameters_and_job_transport_reject_wrong_types() {
    assert!(serde_json::from_value::<MibStartRequest>(
        json!({"onu_id":256,"optical_sn":"TEST_REDACTED","tables":false})
    )
    .is_err());
    assert!(serde_json::from_value::<JobRequest>(json!({"job":"test-job","chunk":-1})).is_err());
    assert!(serde_json::from_value::<HlUpdateRequest>(json!({"target":{"mqtt_mac":"REDACTED","binding_generation":"test"},"kind":"unknown","entry":1,"password":"","active":false})).is_err());
    assert!(
        serde_json::from_value::<NpuSetRequest>(json!({"enabled":"false","bridge":""})).is_err()
    );
    for value in [
        json!({"status":"pending","job":"test-job"}),
        json!({"error":"capacity"}),
    ] {
        let reply: JobStartReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    for value in [
        json!({"status":"pending"}),
        json!({"status":"pending","progress":{"phase":"tables","received":2,"expected":null}}),
        json!({"status":"ready","chunk":0,"chunks":1,"bytes":2,"data":"e30="}),
        json!({"error":"expired"}),
    ] {
        let reply: JobChunkReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<JobChunkReply>(json!({"status":"ready","data":"e30="})).is_err()
    );
    assert!(serde_json::from_value::<NpuSetReply>(json!({"status":"success"})).is_err());
}

#[test]
fn calibration_final_payload_keeps_business_failure_and_omits_absent_fields() {
    for value in [
        json!({"success":true,"output":"calibration applied"}),
        json!({"success":false,"error":"calibration warning"}),
    ] {
        let reply: CalibrationApplied = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<CalibrationApplied>(json!({"error":"worker failure"})).is_err()
    );
}

#[test]
fn npu_policy_requires_all_direction_flags_and_preserves_false() {
    let value = json!({"enabled":false,"bridge":"br-lan","wan_upstream":false,"wan_downstream":false,"onu_bridge":false});
    let request: NpuSetRequest = serde_json::from_value(value.clone()).unwrap();
    assert!(
        !request.enabled && !request.wan_upstream && !request.wan_downstream && !request.onu_bridge
    );
    for field in ["wan_upstream", "wan_downstream", "onu_bridge"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<NpuSetRequest>(missing).is_err());
        let mut invalid = value.clone();
        invalid[field] = json!("0");
        assert!(serde_json::from_value::<NpuSetRequest>(invalid).is_err());
    }
}
