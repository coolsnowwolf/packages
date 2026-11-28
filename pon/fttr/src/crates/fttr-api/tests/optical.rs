use fttr_api::{OpticalAction, OpticalSetRequest, ReplyStatus};

#[test]
fn optical_wire_enums_and_boolean_input_are_strict() {
    for (action, wire) in [
        (OpticalAction::OpticalOn, "optical_on"),
        (OpticalAction::OpticalOff, "optical_off"),
        (OpticalAction::OpticalStatus, "optical_status"),
        (OpticalAction::OpticalReload, "optical_reload"),
        (OpticalAction::OpticalReinitialize, "optical_reinitialize"),
        (OpticalAction::OpticalShutdown, "optical_shutdown"),
    ] {
        let encoded = serde_json::to_value(action).unwrap();
        assert_eq!(encoded, wire);
        assert_eq!(
            serde_json::from_value::<OpticalAction>(encoded).unwrap(),
            action
        );
    }
    assert!(serde_json::from_str::<OpticalAction>("\"optical_typo\"").is_err());
    assert!(serde_json::from_str::<ReplyStatus>("\"ok\"").is_err());
    for invalid in ["{}", "{\"enabled\":\"false\"}", "{\"enabled\":null}"] {
        assert!(serde_json::from_str::<OpticalSetRequest>(invalid).is_err());
    }
    assert!(
        !serde_json::from_str::<OpticalSetRequest>(
            "{\"enabled\":false,\"downstream_tx_enabled\":true,\"fec_enabled\":false}"
        )
        .unwrap()
        .enabled
    );
}
