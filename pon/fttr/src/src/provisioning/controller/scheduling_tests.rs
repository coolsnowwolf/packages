use super::tests::operation;
use super::*;

fn periodic_controller() -> Controller {
    let mut c = Controller::default();
    c.config.interval_seconds = 1800;
    c.config.triggers.insert("periodic".into());
    c
}

#[test]
fn discovery_retry_wait_is_part_of_same_operation_before_periodic_anchor() {
    let mut c = periodic_controller();
    let mut op = operation(OperationPhase::Waiting, false);
    op.started = 100;
    op.capabilities = crate::cwmp::runtime::Discovery::new("auto");
    op.capabilities.tick(
        "test",
        crate::cwmp::identity::Identity {
            optical_vendor: "HWTC".into(),
            optical_serial: "HWTC00000042".into(),
            cwmp_serial: "4857544300000042".into(),
            product_class: "V152".into(),
            software_version: String::new(),
            hardware_version: String::new(),
        },
        1,
        true,
        |_| json!({"error":"queue full","code":"backpressure"}).into(),
    );
    assert_eq!(op.capabilities.view()["state"], "pending");
    c.operations.insert("child".into(), op);
    assert!(!c.periodic_due("child", 100_000));
    assert_eq!(c.operations["child"].completed_at, None);
    c.operations.get_mut("child").unwrap().complete(100_000);
    assert!(!c.periodic_due("child", 101_799));
    assert!(c.periodic_due("child", 101_800));
}

#[test]
fn long_tail_devices_wait_independently_from_completion_without_backlog() {
    let mut c = periodic_controller();
    let mut slow = operation(OperationPhase::WifiReconciling, false);
    slow.started = 100;
    let mut fast = operation(OperationPhase::WifiReconciling, false);
    fast.started = 100;
    fast.device.id = "fast".into();
    fast.complete(200);
    c.operations.insert("child".into(), slow);
    c.operations.insert("fast".into(), fast);

    assert!(!c.periodic_due("fast", 1999));
    assert!(c.periodic_due("fast", 2000));
    assert!(!c.periodic_due("child", 2000));
    assert!(!c.periodic_due("child", 10_000));

    // Completing after several intervals starts a fresh wait, not an overdue run.
    c.operations.get_mut("child").unwrap().complete(10_000);
    assert!(!c.periodic_due("child", 10_000));
    assert!(!c.periodic_due("child", 11_799));
    assert!(c.periodic_due("child", 11_800));
    // Starting a new operation consumes the due state even before it completes.
    c.operations
        .insert("child".into(), operation(OperationPhase::Waiting, false));
    assert!(!c.periodic_due("child", 50_000));
}

#[test]
fn long_tail_waiting_resumes_parameter_discovery_after_management_returns() {
    let mut op = operation(OperationPhase::Waiting, false);
    op.started = now().saturating_sub(86_400);
    RPC.with(|r| *r.borrow_mut() = (Default::default(), Vec::new()));

    advance(&mut op, false);
    assert_eq!(op.stage, OperationPhase::Waiting);
    assert!(op.results.is_empty());
    assert_eq!(op.index, 0);
    RPC.with(|r| assert!(r.borrow().1.is_empty()));

    RPC.with(|r| {
        r.borrow_mut()
            .0
            .extend([json!({"busy":false}), json!({"id":2})])
    });
    advance(&mut op, true);
    assert_eq!(op.stage, OperationPhase::Discovering);
    assert!(op.results.is_empty());
    RPC.with(|r| assert_eq!(r.borrow().1[1]["method"], "GetParameterNames"));
}

#[test]
fn finishing_last_item_is_not_operation_completion() {
    let mut c = periodic_controller();
    let mut op = operation(OperationPhase::Verifying, false);
    op.started = 100;
    op.finish_item(ItemOutcome::Verified, "readback matches");
    assert_eq!(op.stage, OperationPhase::WifiReconciling);
    assert_eq!(op.completed_at, None);
    c.operations.insert("child".into(), op);
    assert!(!c.periodic_due("child", 10_000));

    c.operations.get_mut("child").unwrap().complete(10_000);
    // Later observations or persistence errors do not move the completion time.
    c.operations.get_mut("child").unwrap().complete(10_100);
    assert!(c.periodic_due("child", 11_800));
}

#[test]
fn completion_wait_survives_save_restart_and_config_reload() {
    let mut c = periodic_controller();
    let mut op = operation(OperationPhase::Verifying, false);
    op.started = 100;
    op.finish_item(ItemOutcome::Verified, "readback matches");
    op.complete(3000);
    c.config.devices.push(op.device.clone());
    c.operations.insert("child".into(), op);
    c.dirty = true;
    let mut saved = Vec::new();
    c.persist_history(|data| {
        saved.extend_from_slice(data);
        Ok(())
    });

    let mut restarted = periodic_controller();
    restarted.config.devices = c.config.devices.clone();
    restarted.recover(&saved);
    restarted.replace_config(c.config.clone()).unwrap();
    assert_eq!(restarted.recovered["child"]["completed_at"], 3000);
    assert!(!restarted.periodic_due("child", 4799));
    assert!(restarted.periodic_due("child", 4800));

    // Reload applies a new wait to the same persisted completion timestamp.
    let mut config = restarted.config.clone();
    config.interval_seconds = 3600;
    restarted.replace_config(config).unwrap();
    assert!(!restarted.periodic_due("child", 4800));
    assert!(restarted.periodic_due("child", 6600));
}

#[test]
fn legacy_completed_history_begins_full_wait_at_recovery() {
    let mut c = periodic_controller();
    let before = now();
    c.recover(br#"{"operations":[{"device":"child","state":"completed","started_at":1}]}"#);
    let completed = c.recovered["child"]["completed_at"].as_u64().unwrap();
    assert!(completed >= before && completed <= now());
    assert!(!c.periodic_due("child", completed));
    assert!(!c.periodic_due("child", completed + 1799));
    assert!(c.periodic_due("child", completed + 1800));
    // The migration itself is persisted, so a second restart does not reset it.
    let mut restarted = periodic_controller();
    restarted.recover(c.history().as_bytes());
    assert_eq!(restarted.recovered["child"]["completed_at"], completed);
}

#[test]
fn interrupted_history_requires_reconciliation_before_periodic_sync() {
    let mut c = periodic_controller();
    let op = operation(OperationPhase::Writing, false);
    c.recover(json!({"operations":[op.view()]}).to_string().as_bytes());
    assert_eq!(c.recovered["child"]["state"], "interrupted");
    assert_eq!(c.recovered["child"]["reconcile_required"], true);
    assert!(!c.periodic_due("child", u64::MAX));
}

#[test]
fn new_plan_or_apply_cannot_replace_unresolved_history() {
    for recovered in [false, true] {
        for apply in [false, true] {
            for outcome in ["unknown", "accepted_pending_apply"] {
                let mut c = periodic_controller();
                c.config.enabled = true;
                let mut op = operation(OperationPhase::Completed, false);
                op.results
                    .insert("wifi".into(), json!({"id":"wifi","state":outcome}));
                crate::omci_auth::test_authorize(&op.device.optical_sn);
                c.config.devices.push(op.device.clone());
                if recovered {
                    c.recovered.insert("child".into(), op.view());
                } else {
                    c.operations.insert("child".into(), op);
                }
                let history = c.history();
                let next_id = c.next_id;
                assert!(c
                    .start("child", apply, false)
                    .unwrap_err()
                    .contains("unresolved prior write"));
                assert_eq!(c.history(), history);
                assert_eq!(c.next_id, next_id);
            }
        }
    }
}

#[test]
fn disabled_periodic_and_clock_rollback_do_not_trigger_sync() {
    let mut c = periodic_controller();
    assert!(c.periodic_due("new", 100));
    let mut op = operation(OperationPhase::Completed, false);
    op.complete(3000);
    c.operations.insert("child".into(), op);
    assert!(!c.periodic_due("child", 2999));
    c.config.interval_seconds = 0;
    assert!(!c.periodic_due("new", 100));
    assert!(!c.periodic_due("child", 10_000));
    c.config.interval_seconds = 1800;
    c.config.triggers.clear();
    assert!(!c.periodic_due("new", 100));
    assert!(!c.periodic_due("child", 10_000));
}
