use fttr_api::{Availability, ConfigCheckReply, NpuStatus, NPU_STATUS};
use serde_json::json;

#[test]
fn config_check_retains_business_errors_without_accepting_mixed_replies() {
    for wire in [
        json!({"valid":true,"revision":"test-revision","devices":0}),
        json!({"error":"invalid test configuration"}),
    ] {
        let typed: ConfigCheckReply = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), wire);
    }
    assert!(serde_json::from_value::<ConfigCheckReply>(
        json!({"valid":true,"revision":"test-revision","devices":0,"error":"mixed"})
    )
    .is_err());
}

#[test]
fn npu_json_numbers_and_exact_strings_keep_the_existing_wire_format() {
    let status = NpuStatus {
        total_packets: 9_007_199_254_740_993,
        total_packets_exact: "9007199254740993".into(),
        ..Default::default()
    };
    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(wire["total_packets"].as_u64(), Some(9_007_199_254_740_993));
    assert_eq!(wire["total_packets_exact"], "9007199254740993");
    assert_eq!(serde_json::from_value::<NpuStatus>(wire).unwrap(), status);
    assert_eq!(NPU_STATUS.availability, Availability::Npu);
}
