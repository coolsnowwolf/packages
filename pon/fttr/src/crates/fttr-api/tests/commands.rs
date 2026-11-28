use fttr_api::{ConfigReloadReply, SyncReply, SyncRequest};
use serde_json::{json, Value};

#[test]
fn synchronization_actions_preserve_operations_and_selectors() {
    for action in [
        "plan",
        "apply",
        "management_plan",
        "management_apply",
        "reset_state",
        "reconcile",
        "retry",
    ] {
        for target in ["all", "device_a", "group:group_a"] {
            let request: SyncRequest =
                serde_json::from_value(json!({"action":action,"target":target})).unwrap();
            assert_eq!(
                request.provisioning_request().unwrap(),
                json!({"op":action,"target":target,"management":false,"item":null})
            );
        }
    }
    let request: SyncRequest = serde_json::from_value(
        json!({"action":"apply","target":"group:group_a","management":true,"item":"ssid_a"}),
    )
    .unwrap();
    assert_eq!(
        request.provisioning_request().unwrap(),
        json!({"op":"apply","target":"group:group_a","management":true,"item":"ssid_a"})
    );
    for request in [
        json!({}),
        json!({"action":"reload","target":"all"}),
        json!({"action":"plan","target":"all","management":"false"}),
    ] {
        assert!(serde_json::from_value::<SyncRequest>(request).is_err());
    }
    for target in ["", "-device", "group:", "group:bad/value", "a b"] {
        let request: SyncRequest =
            serde_json::from_value(json!({"action":"plan","target":target})).unwrap();
        assert!(request.provisioning_request().is_err());
    }
}

#[test]
fn command_replies_preserve_success_variants_and_business_errors() {
    for value in [
        json!({"operation_ids":[1,2],"revision":"revision_a"}),
        json!({"started":true,"mode":"plan"}),
        json!({"reset":true,"device":"device_a"}),
        json!({"operation_ids":[3],"mode":"plan","reconcile":true}),
        json!({"operation_ids":[4],"mode":"apply","retry":true}),
        json!({"error":"unknown device"}),
    ] {
        let reply: SyncReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    let error = json!({"error":"invalid configuration"});
    let reply: ConfigReloadReply = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(serde_json::to_value(reply).unwrap(), error);
    assert!(serde_json::from_value::<SyncReply>(Value::Null).is_err());
}
