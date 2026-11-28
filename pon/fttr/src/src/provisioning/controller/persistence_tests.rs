use super::tests::operation;
use super::*;

#[test]
fn reload_pruning_marks_saved_history_pending() {
    let mut c = Controller::default();
    let op = operation(OperationPhase::Completed, false);
    c.config.devices.push(op.device.clone());
    c.operations.insert(op.device.id.clone(), op);
    c.saved = c.history();

    c.replace_config(Config::default()).unwrap();

    assert!(c.dirty);
    c.persist_history(|data| {
        let history: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(history["operations"], json!([]));
        Ok(())
    });
    assert!(!c.dirty);
}

#[test]
fn identity_pruning_marks_saved_history_pending() {
    let mut c = Controller::default();
    let op = operation(OperationPhase::Completed, false);
    let mut device = op.device.clone();
    device.cwmp_id = "pending:H3CT12345678".into();
    c.config.devices.push(device);
    c.recovered.insert(op.device.id.clone(), op.view());
    c.owned.insert(op.device.cwmp_id.clone(), BTreeMap::new());
    c.saved = c.history();

    c.resolve_pending_identities(&crate::cwmp::observations::from_fixture(
        &json!({"devices":{
            "replacement":{"product_class":"HL202-DU-C","serial":"H3CT12345678"}
        }}),
    ));

    assert!(c.dirty);
    c.persist_history(|data| {
        let history: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(history["operations"], json!([]));
        assert_eq!(history["owned"], json!({}));
        Ok(())
    });
    assert!(!c.dirty);
}

#[test]
fn terminal_results_are_saved_when_last_active_operation_completes() {
    let mut c = Controller::default();
    let op = operation(OperationPhase::Verifying, false);
    c.operations.insert(op.device.id.clone(), op);
    c.saved = c.history();

    c.begin_history_tick();
    let op = c.operations.get_mut("child").unwrap();
    op.finish_item(ItemOutcome::Verified, "readback matches");
    op.complete(now());
    let mut written = false;
    c.persist_history(|data| {
        let history: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(history["operations"][0]["state"], "completed");
        assert_eq!(history["operations"][0]["items"][0]["state"], "verified");
        written = true;
        Ok(())
    });

    assert!(written);
    assert!(!c.dirty);
    assert_eq!(c.saved, c.history());
}

#[test]
fn failed_terminal_save_retries_on_idle_tick() {
    let mut c = Controller::default();
    let mut op = operation(OperationPhase::Verifying, false);
    c.saved = json!({"operations":[op.view()],"owned":{}}).to_string();
    op.finish_item(ItemOutcome::Failed, "readback differs");
    op.complete(now());
    c.operations.insert(op.device.id.clone(), op);
    c.dirty = true;
    let previous = c.saved.clone();
    c.persist_history(|_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
    assert!(c.dirty);
    assert_eq!(c.saved, previous);
    assert!(c.failure.is_some());

    c.begin_history_tick();
    let mut written = false;
    c.persist_history(|data| {
        let history: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(history["operations"][0]["state"], "completed");
        assert_eq!(history["operations"][0]["items"][0]["state"], "failed");
        written = true;
        Ok(())
    });

    assert!(written);
    assert!(!c.dirty);
    assert!(c.failure.is_none());
    assert_eq!(c.saved, c.history());
}

#[test]
fn clean_idle_tick_keeps_history_ineligible() {
    let mut c = Controller::default();
    let op = operation(OperationPhase::Completed, false);
    c.operations.insert(op.device.id.clone(), op);
    c.saved = c.history();

    c.begin_history_tick();
    c.persist_history(|_| panic!("clean idle history must not be written"));

    assert!(!c.dirty);
}

#[test]
fn unchanged_active_tick_clears_dirty_without_writing() {
    let mut c = Controller::default();
    let op = operation(OperationPhase::Waiting, false);
    c.operations.insert(op.device.id.clone(), op);
    c.saved = c.history();
    c.failure = Some("unrelated failure".into());

    c.begin_history_tick();
    assert!(c.dirty);
    c.persist_history(|_| panic!("unchanged history must not be written"));

    assert!(!c.dirty);
    assert_eq!(c.failure.as_deref(), Some("unrelated failure"));
}

#[test]
fn ownership_pruning_marks_saved_history_pending_without_operations() {
    let mut c = Controller::default();
    c.owned.insert("removed".into(), BTreeMap::new());
    c.saved = c.history();

    c.retain_configured_state();

    assert!(c.dirty);
}

#[test]
fn initial_history_is_written_without_active_operations() {
    let mut c = Controller::default();
    let mut written = false;

    c.persist_history(|data| {
        let history: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(history["operations"], json!([]));
        written = true;
        Ok(())
    });

    assert!(written);
    assert!(!c.saved.is_empty());
    assert!(!c.dirty);
}

#[test]
fn terminal_history_recovers_after_real_atomic_write_retry() {
    let directory = std::env::temp_dir().join(format!(
        "fttr-history-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let path = directory.join("state.json");
    std::fs::create_dir_all(&path).unwrap();
    let path = path.to_str().unwrap();
    let mut c = Controller::default();
    let op = operation(OperationPhase::Verifying, false);
    c.operations.insert(op.device.id.clone(), op);
    c.saved = c.history();
    c.begin_history_tick();
    let op = c.operations.get_mut("child").unwrap();
    op.finish_item(ItemOutcome::Verified, "readback matches");
    op.complete(now());

    c.persist_history(|data| super::super::storage::write_atomic(path, data, false));
    assert!(c.dirty);
    assert!(c.failure.is_some());
    std::fs::remove_dir(path).unwrap();
    c.begin_history_tick();
    c.persist_history(|data| super::super::storage::write_atomic(path, data, false));
    let mut restarted = Controller::default();
    restarted.recover(&std::fs::read(path).unwrap());
    std::fs::remove_dir_all(directory).unwrap();

    assert!(!c.dirty);
    assert!(c.failure.is_none());
    assert_eq!(restarted.recovered["child"]["state"], "completed");
    assert_eq!(
        restarted.recovered["child"]["items"][0]["state"],
        "verified"
    );
}
