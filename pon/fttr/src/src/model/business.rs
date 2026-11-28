pub use fttr_api::{BusinessPhase, BusinessStatus};

#[cfg(test)]
mod tests {
    use super::BusinessStatus;

    #[test]
    fn deserialization_rejects_unrecognized_or_legacy_worker_state() {
        for value in [
            serde_json::json!("Verified (generation 1)"),
            serde_json::json!({"phase": "Verified", "generation": 1, "error": null}),
            serde_json::json!({"phase": "invented", "generation": 1, "error": null}),
        ] {
            let result = serde_json::from_value::<BusinessStatus>(value);
            assert!(result.is_err());
        }
    }
}
