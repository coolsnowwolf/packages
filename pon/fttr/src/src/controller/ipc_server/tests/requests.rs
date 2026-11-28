use super::*;
use crate::controller::optical_owner::{
    OpticalActor, OpticalBackend, OpticalClient, OpticalCommand, OpticalRequest,
};
use fttr_api::OpticalState;

fn optical_handler(optics: OpticalClient) -> impl Fn(UnixStream, &str) + Send + Sync + 'static {
    crate::platform::h3c_hm2004_du::runtime::ipc_handler(
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(crate::pon_engine::test_engine())),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        optics,
    )
}

#[test]
fn production_dispatch_retains_status_and_adapter_routes() {
    let (requests, _receiver) = std::sync::mpsc::sync_channel(2);
    let handler = optical_handler(OpticalClient {
        platform: crate::platform::context::PlatformContext::test_context(),
        requests,
        hardware_ready: Arc::new(AtomicBool::new(false)),
        stopping: Arc::new(AtomicBool::new(false)),
    });
    let server = Server::new(5, handler);

    let responses: Vec<serde_json::Value> =
        ["STATUS", "ACS {", "PROVISIONING {", "HL202 {", "MIB {"]
            .into_iter()
            .map(|command| {
                serde_json::from_str(&server.request(format!("{command}\n").as_bytes())).unwrap()
            })
            .collect();

    assert!(responses[0]["switch_ports"].is_array());
    for response in &responses[1..] {
        assert!(!response["error"].is_null(), "{response}");
    }
}

#[test]
fn removed_ipc_commands_are_unsupported_without_publication() {
    let (requests, _receiver) = std::sync::mpsc::sync_channel(2);
    let server = Server::new(
        2,
        optical_handler(OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests,
            hardware_ready: Arc::new(AtomicBool::new(false)),
            stopping: Arc::new(AtomicBool::new(false)),
        }),
    );
    // No broker is needed: the real production handler rejects these locally.
    for request in [
        "EXEC 02AABBCCDDEE fixture\n",
        "SUBDEV 02AABBCCDDEE status\n",
    ] {
        let response: serde_json::Value =
            serde_json::from_str(&server.request(request.as_bytes())).unwrap();
        assert_eq!(response["error"], "unsupported command");
    }
}

#[derive(Default)]
struct TestOptics {
    ready: bool,
    tx: bool,
    pending: bool,
    write_failure: bool,
    read_failure: bool,
    mismatch: bool,
    quiesce_failure: bool,
    events: Vec<&'static str>,
    desired: bool,
    save_failure: bool,
    enable_failure_after_write: bool,
    saved_settings: Option<fttr_api::OpticalSettings>,
    invalid_calibration: bool,
}

impl OpticalBackend for TestOptics {
    fn validate_calibration(&self) -> io::Result<()> {
        if self.invalid_calibration {
            Err(io::Error::other("selected calibration is invalid"))
        } else {
            Ok(())
        }
    }
    fn save_settings(&mut self, settings: fttr_api::OpticalSettings) -> io::Result<()> {
        self.save(settings.enabled)?;
        self.saved_settings = Some(settings);
        Ok(())
    }
    fn desired(&self) -> io::Result<bool> {
        Ok(self.desired)
    }
    fn save(&mut self, enabled: bool) -> io::Result<()> {
        if self.save_failure {
            return Err(io::Error::other("read-only configuration"));
        }
        self.desired = enabled;
        Ok(())
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn cleanup_pending(&self) -> bool {
        self.pending
    }
    fn quiesce(&mut self) -> io::Result<()> {
        self.events.push("quiesce");
        self.pending = true;
        if self.quiesce_failure {
            Err(io::Error::other("carrier unavailable"))
        } else {
            Ok(())
        }
    }
    fn set_tx(&mut self, enabled: bool) -> io::Result<()> {
        self.events.push(if enabled { "enable" } else { "disable" });
        if self.write_failure {
            return Err(io::Error::other("write unavailable"));
        }
        if !self.mismatch {
            self.tx = enabled;
        }
        if enabled && self.enable_failure_after_write {
            return Err(io::Error::other("calibration failed after release"));
        }
        Ok(())
    }
    fn read_tx(&mut self) -> io::Result<bool> {
        self.events.push("read");
        if self.read_failure {
            Err(io::Error::other("read unavailable"))
        } else {
            Ok(self.tx)
        }
    }
}

#[test]
fn calibration_apply_while_off_does_not_enable_and_invalid_selection_is_reported() {
    let mut actor = OpticalActor {
        state: OpticalState::Disabled,
    };
    let mut backend = TestOptics {
        ready: true,
        ..Default::default()
    };
    let applied = actor.handle(OpticalCommand::Reinitialize, &mut backend);
    assert_eq!(applied["status"], "success");
    assert_eq!(applied["desired_enabled"], false);
    assert_eq!(applied["actor_state"], "disabled");
    assert!(!backend.events.contains(&"enable"));
    assert_eq!(backend.events, ["quiesce", "disable", "read"]);

    backend.events.clear();
    backend.invalid_calibration = true;
    let rejected = actor.handle(OpticalCommand::Reinitialize, &mut backend);
    assert_eq!(rejected["status"], "error");
    assert_eq!(rejected["error"]["code"], "Configuration");
    assert!(!backend.tx);
    assert!(!backend.desired);
    assert!(!backend.events.contains(&"enable"));
}

#[test]
fn grouped_optical_settings_are_saved_by_the_owner_and_expired_requests_never_save() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        desired: true,
        tx: true,
        ..Default::default()
    };
    let settings = fttr_api::OpticalSettings {
        enabled: true,
        downstream_tx_enabled: false,
        fec_enabled: true,
    };
    let (reply, _) = std::sync::mpsc::sync_channel(1);
    let mut request = OpticalRequest {
        command: OpticalCommand::Set(settings),
        reply,
        deadline: Instant::now() + Duration::from_secs(1),
    };
    assert_eq!(
        actor.dispatch(&request, &mut backend, &AtomicBool::new(false))["status"],
        "success"
    );
    assert_eq!(backend.saved_settings, Some(settings));
    // A settings-only update does not unnecessarily reset the optical owner.
    assert!(!backend.events.contains(&"quiesce"));
    request.command = OpticalCommand::Set(fttr_api::OpticalSettings {
        enabled: false,
        ..settings
    });
    request.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        actor.dispatch(&request, &mut backend, &AtomicBool::new(false))["status"],
        "error"
    );
    assert_eq!(backend.saved_settings, Some(settings));
    assert!(backend.tx);
}

#[test]
fn optical_ipc_rejects_requests_before_hardware_ready_without_queueing() {
    let (requests, receiver) = std::sync::mpsc::sync_channel(2);
    let server = Server::new(
        3,
        optical_handler(OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests: requests.clone(),
            hardware_ready: Arc::new(AtomicBool::new(false)),
            stopping: Arc::new(AtomicBool::new(false)),
        }),
    );

    for command in ["OPTICAL ON\n", "OPTICAL OFF\n", "OPTICAL STATUS\n"] {
        let response: serde_json::Value =
            serde_json::from_str(&server.request(command.as_bytes())).unwrap();

        assert_eq!(response["error"]["code"], "NotConnected");
        assert_eq!(response["hardware_ready"], false);
        assert!(response["tx_enabled"].is_null());
        assert!(response["desired_enabled"].is_null());
        assert_eq!(response["cleanup_pending"], false);
        assert_eq!(response["discovery_active"], false);
        assert!(response["action"].as_str().unwrap().starts_with("optical_"));
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn optical_owner_rechecks_readiness_before_hardware_access() {
    let mut actor = OpticalActor {
        state: OpticalState::Unknown,
    };
    let mut backend = TestOptics::default();

    let response = actor.handle(OpticalCommand::Off, &mut backend);

    assert_eq!(response["error"]["code"], "NotConnected");
    assert!(backend.events.is_empty());
}

#[test]
fn optical_saved_intent_survives_shutdown_and_failed_enable() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        tx: true,
        desired: true,
        ..TestOptics::default()
    };
    let stopped = actor.handle(OpticalCommand::Shutdown, &mut backend);
    assert_eq!(stopped["desired_enabled"], true);
    assert_eq!(stopped["tx_enabled"], false);
    assert!(backend.desired, "service stop must not save off");
    backend.write_failure = true;
    let failed = actor.handle(OpticalCommand::On, &mut backend);
    assert_eq!(failed["status"], "error");
    assert_eq!(failed["desired_enabled"], true);
    assert_eq!(failed["actor_state"], "unknown");
    assert_eq!(failed["discovery_active"], false);
    assert_eq!(failed["tx_enabled"], false);
    assert!(backend
        .events
        .ends_with(&["enable", "disable", "read", "quiesce"]));
}

#[test]
fn optical_save_failure_retains_actual_state_and_reload_is_idempotent() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        tx: true,
        desired: true,
        save_failure: true,
        ..TestOptics::default()
    };
    let failed = actor.handle(OpticalCommand::Off, &mut backend);
    assert_eq!(failed["status"], "error");
    assert_eq!(failed["desired_enabled"], true);
    assert_eq!(failed["tx_enabled"], true);
    assert_eq!(failed["discovery_active"], true);
    assert_eq!(backend.events, ["read"]);
    backend.events.clear();
    let reloaded = actor.handle(OpticalCommand::Reload, &mut backend);
    assert_eq!(reloaded["action"], "optical_reload");
    assert_eq!(reloaded["status"], "success");
    assert!(backend.events.iter().all(|event| *event == "read"));
    backend.desired = false;
    let reloaded = actor.handle(OpticalCommand::Reload, &mut backend);
    assert_eq!(reloaded["desired_enabled"], false);
    assert_eq!(reloaded["tx_enabled"], false);
    assert_eq!(reloaded["actor_state"], "disabled");
}

#[test]
fn optical_stop_before_ready_cancels_later_enable_requests() {
    let (requests, receiver) = std::sync::mpsc::sync_channel::<OpticalRequest>(2);
    let stopping = Arc::new(AtomicBool::new(false));
    let client = OpticalClient {
        platform: crate::platform::context::PlatformContext::test_context(),
        requests,
        hardware_ready: Arc::new(AtomicBool::new(false)),
        stopping: stopping.clone(),
    };
    let owner = thread::spawn(move || {
        let request = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut actor = OpticalActor {
            state: OpticalState::Unknown,
        };
        let mut backend = TestOptics {
            desired: true,
            ..TestOptics::default()
        };
        request
            .reply
            .send(actor.handle(request.command, &mut backend))
            .unwrap();
    });
    let response = client.request(OpticalCommand::Shutdown).unwrap();
    assert!(stopping.load(Ordering::Acquire));
    assert_eq!(response["tx_enabled"], false);
    assert_eq!(response["desired_enabled"], true);
    assert!(client.request(OpticalCommand::On).is_err());
    owner.join().unwrap();
}

#[test]
fn optical_failed_quiesce_resets_instead_of_reenabling() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        tx: true,
        quiesce_failure: true,
        ..TestOptics::default()
    };
    let response = actor.handle(OpticalCommand::On, &mut backend);
    assert_eq!(response["status"], "error");
    assert_eq!(response["desired_enabled"], true);
    assert_eq!(response["tx_enabled"], false);
    assert_eq!(response["actor_state"], "unknown");
    assert_eq!(response["discovery_active"], false);
    assert_eq!(backend.events, ["quiesce", "disable", "read"]);
}

#[test]
fn optical_failed_quiesce_cannot_skip_off_or_shutdown_hardware_write() {
    for command in [OpticalCommand::Off, OpticalCommand::Shutdown] {
        let mut actor = OpticalActor {
            state: OpticalState::Enabled,
        };
        let mut backend = TestOptics {
            ready: true,
            desired: true,
            tx: true,
            quiesce_failure: true,
            ..Default::default()
        };
        let response = actor.handle(command, &mut backend);
        assert_eq!(response["status"], "error");
        assert_eq!(response["tx_enabled"], false);
        assert!(
            !backend.tx,
            "failed carrier cleanup must not leave transmission on"
        );
        assert_eq!(backend.desired, matches!(command, OpticalCommand::Shutdown));
        assert_eq!(backend.events, ["quiesce", "disable", "read"]);
    }
}

#[test]
fn optical_partial_enable_resets_hardware_and_expired_queue_never_saves() {
    let mut actor = OpticalActor {
        state: OpticalState::Disabled,
    };
    let mut backend = TestOptics {
        ready: true,
        enable_failure_after_write: true,
        ..TestOptics::default()
    };
    let failed = actor.handle(OpticalCommand::On, &mut backend);
    assert_eq!(failed["desired_enabled"], true);
    assert_eq!(failed["tx_enabled"], false);
    assert_eq!(failed["actor_state"], "unknown");
    assert!(backend.events.starts_with(&["enable", "disable", "read"]));
    backend.events.clear();
    let (reply, _receiver) = std::sync::mpsc::sync_channel(1);
    let request = OpticalRequest {
        command: OpticalCommand::Off,
        reply,
        deadline: Instant::now() - Duration::from_secs(1),
    };
    let expired = actor.dispatch(&request, &mut backend, &AtomicBool::new(false));
    assert_eq!(expired["error"]["code"], "TimedOut");
    assert!(backend.desired);
    assert!(backend.events.is_empty());
}

#[test]
fn optical_failures_pause_discovery_without_false_success() {
    for command in [
        OpticalCommand::On,
        OpticalCommand::Off,
        OpticalCommand::Status,
    ] {
        for failure in ["write", "read", "mismatch", "quiesce"] {
            if matches!(command, OpticalCommand::Status) && failure != "read" {
                continue;
            }
            if matches!(command, OpticalCommand::On) && failure == "quiesce" {
                continue;
            }
            let mut actor = OpticalActor {
                state: OpticalState::Enabled,
            };
            let mut backend = TestOptics {
                ready: true,
                tx: matches!(command, OpticalCommand::Off | OpticalCommand::Status),
                write_failure: failure == "write",
                read_failure: failure == "read",
                mismatch: failure == "mismatch",
                quiesce_failure: failure == "quiesce",
                ..TestOptics::default()
            };

            let response = actor.handle(command, &mut backend);

            assert_eq!(response["status"], "error", "{failure}: {response}");
            assert_eq!(response["actor_state"], "unknown");
            assert_eq!(response["discovery_active"], false);
            assert!(backend.pending);
            if matches!(command, OpticalCommand::Off) {
                assert_eq!(&backend.events[..2], &["quiesce", "disable"]);
            }
        }
    }
}

#[test]
fn optical_status_closes_unconfirmed_hardware_instead_of_promoting_paused_actor() {
    let mut actor = OpticalActor {
        state: OpticalState::Unknown,
    };
    let mut backend = TestOptics {
        ready: true,
        tx: true,
        ..TestOptics::default()
    };

    let response = actor.handle(OpticalCommand::Status, &mut backend);

    assert_eq!(response["status"], "error");
    assert_eq!(response["tx_enabled"], false);
    assert_eq!(response["actor_state"], "unknown");
    assert_eq!(response["discovery_active"], false);
    assert_eq!(backend.events, ["read", "disable", "read", "quiesce"]);
}

#[test]
fn optical_status_quiesces_when_hardware_is_unexpectedly_disabled() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        ..TestOptics::default()
    };

    let response = actor.handle(OpticalCommand::Status, &mut backend);

    assert_eq!(response["tx_enabled"], false);
    assert_eq!(response["actor_state"], "disabled");
    assert_eq!(response["discovery_active"], false);
    assert!(backend.pending);
}

#[test]
fn optical_enable_holds_discovery_until_pending_cleanup_completes() {
    let mut actor = OpticalActor {
        state: OpticalState::Disabled,
    };
    let mut backend = TestOptics {
        ready: true,
        pending: true,
        ..TestOptics::default()
    };

    let response = actor.handle(OpticalCommand::On, &mut backend);

    assert_eq!(response["status"], "success");
    assert_eq!(response["actor_state"], "enabled");
    assert_eq!(response["tx_enabled"], true);
    assert_eq!(response["discovery_active"], false);
    assert_eq!(backend.events, ["enable", "read"]);
}

#[test]
fn optical_status_resumes_discovery_only_after_cleanup_is_complete() {
    let mut actor = OpticalActor {
        state: OpticalState::Enabled,
    };
    let mut backend = TestOptics {
        ready: true,
        tx: true,
        pending: false,
        ..TestOptics::default()
    };

    let response = actor.handle(OpticalCommand::Status, &mut backend);

    assert_eq!(response["tx_enabled"], true);
    assert_eq!(response["discovery_active"], true);
}

#[test]
fn optical_ipc_returns_owner_read_failure_without_cached_tx_state() {
    let (requests, receiver) = std::sync::mpsc::sync_channel::<OpticalRequest>(2);
    let server = Server::new(
        1,
        optical_handler(OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests,
            hardware_ready: Arc::new(AtomicBool::new(true)),
            stopping: Arc::new(AtomicBool::new(false)),
        }),
    );
    let owner = thread::spawn(move || {
        let mut actor = OpticalActor {
            state: OpticalState::Enabled,
        };
        let mut backend = TestOptics {
            ready: true,
            tx: true,
            read_failure: true,
            ..TestOptics::default()
        };
        let request = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        request
            .reply
            .send(actor.handle(request.command, &mut backend))
            .unwrap();
    });

    let response: serde_json::Value =
        serde_json::from_str(&server.request(b"OPTICAL STATUS\n")).unwrap();

    assert_eq!(response["status"], "error");
    assert!(response["tx_enabled"].is_null());
    assert_eq!(response["actor_state"], "unknown");
    assert_eq!(response["discovery_active"], false);
    owner.join().unwrap();
}

#[test]
fn optical_quiesce_cancels_ranging_and_live_onus_even_when_carrier_fails() {
    use crate::controller::service::{Services, State};
    use crate::model::BusinessPhase;
    use crate::platform::h3c_hm2004_du::{activation::RangingSession, runtime::RuntimeOptics};
    use std::collections::{BTreeMap, BTreeSet};

    let now = Instant::now();
    let session = |id| RangingSession {
        id,
        serial: [0; 8],
        started: now,
        armed: true,
        matched: true,
        measured: None,
        ranged: None,
        allocated: false,
        confirmed: false,
        last_valid_rx: now,
    };
    let mut ranging = Some(session(1));
    let mut online = BTreeMap::from([(2, session(2))]);
    let mut pending_cleanup = BTreeSet::new();
    let states = Arc::new(Mutex::new(BTreeMap::from([
        (1, State::default()),
        (2, State::default()),
    ])));
    let mut services = Services::new(
        crate::platform::context::PlatformContext::test_context(),
        states.clone(),
    );
    let engine = Mutex::new(crate::pon_engine::test_engine());
    let mut backend = RuntimeOptics {
        ready: true,
        ranging: &mut ranging,
        online: &mut online,
        pending_cleanup: &mut pending_cleanup,
        services: &mut services,
        engine: &engine,
    };
    let mut lowered = Vec::new();

    let result = backend.quiesce_with(|id| {
        lowered.push(id);
        if id == 1 {
            Err(io::Error::other("carrier unavailable"))
        } else {
            Ok(())
        }
    });

    assert!(result.is_err());
    assert!(ranging.is_none());
    assert!(online.is_empty());
    assert_eq!(pending_cleanup, BTreeSet::from([1, 2]));
    assert_eq!(lowered, [1, 2]);
    for state in states.lock().unwrap().values() {
        assert_eq!(state.phase, BusinessPhase::Offline);
    }
}

#[test]
fn optical_queue_rejects_overflow_and_disconnected_owner() {
    let (requests, receiver) = std::sync::mpsc::sync_channel(1);
    let client = OpticalClient {
        platform: crate::platform::context::PlatformContext::test_context(),
        requests,
        hardware_ready: Arc::new(AtomicBool::new(true)),
        stopping: Arc::new(AtomicBool::new(false)),
    };
    let (reply, _response) = std::sync::mpsc::sync_channel(1);
    client
        .requests
        .try_send(OpticalRequest {
            command: OpticalCommand::Status,
            reply,
            deadline: Instant::now() + Duration::from_secs(5),
        })
        .unwrap();

    let full = client.request(OpticalCommand::On).unwrap_err();
    drop(receiver);
    let disconnected = client.request(OpticalCommand::Off).unwrap_err();

    assert_eq!(full.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(disconnected.kind(), io::ErrorKind::NotConnected);
}

#[test]
fn concurrent_optical_ipc_transitions_are_executed_by_one_owner() {
    let (requests, receiver) = std::sync::mpsc::sync_channel::<OpticalRequest>(2);
    let server = Server::new(
        2,
        optical_handler(OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests,
            hardware_ready: Arc::new(AtomicBool::new(true)),
            stopping: Arc::new(AtomicBool::new(false)),
        }),
    );
    let (received, first_received) = channel();
    let owner = thread::spawn(move || {
        let mut actor = OpticalActor {
            state: OpticalState::Disabled,
        };
        let mut backend = TestOptics {
            ready: true,
            ..TestOptics::default()
        };
        let first = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        received.send(()).unwrap();
        let second = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        for request in [first, second] {
            request
                .reply
                .send(actor.handle(request.command, &mut backend))
                .unwrap();
        }
        backend.events
    });
    let mut on = server.connect();
    on.write_all(b"OPTICAL ON\n").unwrap();
    signal(&first_received);
    let mut off = server.connect();

    off.write_all(b"OPTICAL OFF\n").unwrap();
    let enabled: serde_json::Value = serde_json::from_str(&reply(on)).unwrap();
    let disabled: serde_json::Value = serde_json::from_str(&reply(off)).unwrap();

    assert_eq!(enabled["tx_enabled"], true);
    assert_eq!(enabled["discovery_active"], true);
    assert_eq!(disabled["tx_enabled"], false);
    assert_eq!(disabled["actor_state"], "disabled");
    assert_eq!(disabled["discovery_active"], false);
    assert_eq!(
        owner.join().unwrap(),
        ["enable", "read", "quiesce", "disable", "read"]
    );
}

#[test]
fn request_boundary_rejects_eof_invalid_utf8_and_oversized_lines() {
    for input in [
        b"STATUS".to_vec(),
        vec![255, b'\n'],
        vec![b'x'; MAX_REQUEST_BYTES],
    ] {
        let (mut client, stream) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            client.write_all(&input).unwrap();
            client.shutdown(Shutdown::Write).unwrap();
        });

        let result = read_request(Connection {
            stream,
            accepted: Instant::now(),
        });

        assert!(result.is_err());
        writer.join().unwrap();
    }
}

#[test]
fn request_at_byte_limit_retains_first_line_routing() {
    let (mut client, stream) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        let mut line = vec![b'x'; MAX_REQUEST_BYTES - 1];
        line.push(b'\n');
        client.write_all(&line).unwrap();
    });

    let request = read_request(Connection {
        stream,
        accepted: Instant::now(),
    })
    .unwrap();

    assert_eq!(request.line.len(), MAX_REQUEST_BYTES);
    writer.join().unwrap();
}

#[test]
fn expired_request_deadline_rejects_queued_connections() {
    let (_client, stream) = UnixStream::pair().unwrap();

    let result = read_request(Connection {
        stream,
        accepted: Instant::now() - SOCKET_TIMEOUT,
    });

    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn stalled_request_read_expires_at_the_total_deadline() {
    let (_client, stream) = UnixStream::pair().unwrap();

    let result = read_request(Connection {
        stream,
        accepted: Instant::now() - SOCKET_TIMEOUT + Duration::from_millis(50),
    });

    assert!(matches!(
        result.err().unwrap().kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
}

#[test]
fn busy_is_returned_when_all_intake_slots_are_stalled() {
    let count = INTAKE_WORKERS * 2 + 1;
    let server = Server::new(count, |_, _| panic!("incomplete request reached handler"));
    let (responded, responses) = channel();
    let mut clients = Vec::new();
    let mut readers = Vec::new();
    for _ in 0..count {
        let client = server.connect();
        let stream = client.try_clone().unwrap();
        let responded = responded.clone();
        readers.push(thread::spawn(move || {
            responded.send(reply(stream)).unwrap()
        }));
        clients.push(client);
    }

    let response = responses.recv_timeout(Duration::from_secs(2)).unwrap();

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["error"]["code"],
        "busy"
    );
    for client in clients {
        client.shutdown(Shutdown::Both).unwrap();
    }
    for reader in readers {
        reader.join().unwrap();
    }
}
