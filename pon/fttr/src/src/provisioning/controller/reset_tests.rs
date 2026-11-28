use super::*;
use crate::provisioning::controller::tests::operation;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    controller: Controller,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fttr-reset-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut controller = Controller::default();
        controller.resets.directory = root.join("intents").to_str().unwrap().into();
        controller.resets.history = root.join("history.json").to_str().unwrap().into();
        controller.resets.wan = root.join("wan").to_str().unwrap().into();
        controller.resets.wifi = root.join("wifi").to_str().unwrap().into();
        let op = operation(OperationPhase::Writing, false);
        controller.config.devices.push(op.device.clone());
        controller.operations.insert("child".into(), op);
        controller.recovered.insert(
            "other".into(),
            json!({"device":"other","optical_sn":"OTHER","state":"completed"}),
        );
        for directory in [&controller.resets.wan, &controller.resets.wifi] {
            storage::write_atomic(&storage::registry_path(directory, "id"), b"owned", true)
                .unwrap();
            storage::write_atomic(
                &storage::registry_path(directory, "other"),
                b"untouched",
                true,
            )
            .unwrap();
        }
        Self { controller, root }
    }
    fn acknowledge(&self) {
        RPC.with(|rpc| *rpc.borrow_mut() = (vec![json!({"reset":true})].into(), Vec::new()));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn reset_retries_each_persistence_failure_without_touching_other_devices() {
    for stage in ["intent", "history", "wan", "wifi"] {
        let mut fixture = Fixture::new();
        let c = &fixture.controller;
        let path = match stage {
            "intent" => c.resets.directory.clone(),
            "history" => c.resets.history.clone(),
            "wan" => storage::registry_path(&c.resets.wan, "id"),
            "wifi" => storage::registry_path(&c.resets.wifi, "id"),
            _ => unreachable!(),
        };
        if stage == "intent" {
            std::fs::write(&path, b"blocked").unwrap();
        } else {
            if stage != "history" {
                std::fs::remove_file(&path).unwrap();
            }
            std::fs::create_dir(&path).unwrap();
        }
        fixture.acknowledge();
        let old_history = fixture.controller.history();

        let result = fixture.controller.reset_device("child");

        assert!(result.unwrap_err().starts_with("reset pending"));
        assert!(fixture.controller.resets.blocks("H3CT12345678"));
        assert!(fixture.controller.start("child", false, false).is_err());
        assert_eq!(
            fixture.controller.snapshot()["devices"][0]["provisioning"]["state"],
            "reset_pending"
        );
        if stage == "intent" {
            assert!(fixture.controller.operations.contains_key("child"));
        } else {
            fixture.controller.operations.clear();
            fixture.controller.resets.pending.clear();
            fixture.controller.resets.blocked.clear();
            fixture.controller.load_reset_intents();
            fixture.controller.recover(old_history.as_bytes());
            assert!(!fixture.controller.recovered.contains_key("child"));
            assert!(fixture.controller.start("child", false, false).is_err());
        }
        if stage == "intent" {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::remove_dir(&path).unwrap();
        }
        fixture.acknowledge();
        fixture.controller.reset_device("child").unwrap();
        assert!(!fixture.controller.resets.blocks("H3CT12345678"));
        assert!(fixture.controller.operations.is_empty());
        for directory in [
            &fixture.controller.resets.wan,
            &fixture.controller.resets.wifi,
        ] {
            assert!(!std::path::Path::new(&storage::registry_path(directory, "id")).exists());
            assert_eq!(
                std::fs::read(storage::registry_path(directory, "other")).unwrap(),
                b"untouched"
            );
        }
        let history: Value =
            serde_json::from_slice(&std::fs::read(&fixture.controller.resets.history).unwrap())
                .unwrap();
        assert_eq!(history["operations"][0]["device"], "other");
        fixture.acknowledge();
        assert_eq!(
            fixture.controller.reset_device("child").unwrap()["reset"],
            true
        );
    }
}

#[cfg(feature = "acs")]
#[test]
fn acs_failure_is_pending_and_restart_resumes_before_old_operation_can_start() {
    let mut fixture = Fixture::new();
    let old_history = fixture.controller.history();
    RPC.with(|rpc| *rpc.borrow_mut() = (vec![json!({"error":"save failed"})].into(), Vec::new()));

    let result = fixture.controller.reset_device("child");

    assert!(result.unwrap_err().contains("reset pending"));
    let marker = fixture.controller.resets.path("H3CT12345678");
    assert_eq!(
        std::fs::metadata(marker).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fixture.controller.operations.clear();
    fixture.controller.resets.pending.clear();
    fixture.controller.resets.blocked.clear();
    fixture.controller.load_reset_intents();
    fixture.controller.recover(old_history.as_bytes());
    assert!(!fixture.controller.recovered.contains_key("child"));
    assert!(fixture.controller.start("child", false, false).is_err());
    fixture.acknowledge();
    fixture.controller.resume_resets();
    assert!(!fixture.controller.resets.blocks("H3CT12345678"));
}

#[test]
fn corrupt_or_mismatched_intent_fails_closed_without_clearing_state() {
    for data in [
        b"bad json".as_slice(),
        b"{\"optical_sn\":\"OTHER\",\"cwmp_id\":\"id\"}".as_slice(),
        b"{\"optical_sn\":\"H3CT12345678\",\"cwmp_id\":\"foreign\"}".as_slice(),
        &[b' '; 4097],
    ] {
        let mut fixture = Fixture::new();
        storage::write_atomic(&fixture.controller.resets.path("H3CT12345678"), data, true).unwrap();

        fixture.controller.load_reset_intents();

        assert!(fixture.controller.reset_device("child").is_err());
        assert!(fixture.controller.start("child", false, false).is_err());
        assert!(fixture.controller.operations.contains_key("child"));
        assert!(std::path::Path::new(&storage::registry_path(
            &fixture.controller.resets.wan,
            "id"
        ))
        .exists());
    }
}

#[test]
fn startup_leaves_unconfigured_reset_intents_untouched() {
    let mut fixture = Fixture::new();
    let path = fixture.controller.resets.path("OTHER");
    storage::write_atomic(&path, b"unconfigured marker", true).unwrap();

    fixture.controller.load_reset_intents();
    fixture.controller.resume_resets();

    assert!(fixture.controller.resets.pending.is_empty());
    assert!(fixture.controller.resets.blocked.is_empty());
    assert_eq!(std::fs::read(path).unwrap(), b"unconfigured marker");
}

#[test]
fn reload_blocks_retained_intents_before_start_without_replaying_cleanup() {
    for data in [
        br#"{"optical_sn":"H3CT12345678","cwmp_id":"id"}"#.as_slice(),
        b"bad json",
    ] {
        let mut fixture = Fixture::new();
        let c = &mut fixture.controller;
        let target = c.config.devices.pop().unwrap();
        let mut other = operation(OperationPhase::Completed, false);
        other.device.id = "other".into();
        other.device.optical_sn = "OTHER".into();
        other.device.cwmp_id = "other".into();
        c.config.devices.push(other.device.clone());
        c.operations.clear();
        c.recovered.insert("other".into(), other.view());
        c.owned.insert("other".into(), BTreeMap::new());
        let history = c.history();
        let marker = c.resets.path(&target.optical_sn);
        let unrelated = c.resets.path("UNCONFIGURED");
        storage::write_atomic(&marker, data, true).unwrap();
        storage::write_atomic(&unrelated, b"untouched", true).unwrap();
        storage::write_atomic(&c.resets.history, history.as_bytes(), true).unwrap();
        c.load_reset_intents();
        c.recover(history.as_bytes());
        c.retain_configured_state();
        c.resume_resets();
        assert!(!c.resets.blocks(&target.optical_sn));
        let mut config = c.config.clone();
        config.enabled = true;
        config.automatic = true;
        config.devices.push(target.clone());

        c.replace_config(config).unwrap();

        assert!(c.resets.blocks(&target.optical_sn));
        assert_eq!(
            c.snapshot()["devices"][1]["provisioning"]["state"],
            "reset_pending"
        );
        for (selected, apply) in [("all", true), ("child", true), ("child", false)] {
            assert_eq!(
                c.start(selected, apply, false).unwrap_err(),
                "device reset pending; retry reset before synchronization"
            );
        }
        assert_eq!(c.next_id, 1);
        assert!(c.operations.is_empty());
        assert_eq!(c.history(), history);
        assert_eq!(
            std::fs::read(&c.resets.history).unwrap(),
            history.as_bytes()
        );
        assert_eq!(std::fs::read(&marker).unwrap(), data);
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"untouched");
        for directory in [&c.resets.wan, &c.resets.wifi] {
            assert_eq!(
                std::fs::read(storage::registry_path(directory, "other")).unwrap(),
                b"untouched"
            );
            assert_eq!(
                std::fs::read(storage::registry_path(directory, "id")).unwrap(),
                b"owned"
            );
        }
    }
}

#[test]
fn completion_failure_keeps_device_pending_until_marker_removal_is_durable() {
    let mut fixture = Fixture::new();
    let serial = "H3CT12345678";
    let intent = Intent {
        optical_sn: serial.into(),
        cwmp_id: "id".into(),
    };
    storage::write_atomic(
        &fixture.controller.resets.path(serial),
        &serde_json::to_vec(&intent).unwrap(),
        true,
    )
    .unwrap();
    fixture
        .controller
        .resets
        .pending
        .insert(serial.into(), intent);
    let directory = &fixture.controller.resets.directory;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500)).unwrap();

    let result = fixture.controller.complete_reset(serial);

    std::fs::set_permissions(
        &fixture.controller.resets.directory,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(result.is_err());
    assert!(fixture.controller.resets.blocks(serial));
    fixture.controller.complete_reset(serial).unwrap();
    assert!(!fixture.controller.resets.blocks(serial));
}
