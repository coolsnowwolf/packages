//! Platform-independent optical request queue and single-owner transition policy.
use crate::platform::context::PlatformContext;
use fttr_api::{OpticalAction, OpticalError, OpticalReply, OpticalState, ReplyStatus};
use serde_json::Value;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) enum OpticalCommand {
    Set(fttr_api::OpticalSettings),
    On,
    Off,
    Status,
    Reload,
    Reinitialize,
    Shutdown,
}
impl OpticalCommand {
    pub(crate) fn action(self) -> OpticalAction {
        match self {
            Self::Set(_) | Self::Reload => OpticalAction::OpticalReload,
            Self::On => OpticalAction::OpticalOn,
            Self::Off => OpticalAction::OpticalOff,
            Self::Status => OpticalAction::OpticalStatus,
            Self::Reinitialize => OpticalAction::OpticalReinitialize,
            Self::Shutdown => OpticalAction::OpticalShutdown,
        }
    }
}
pub(crate) trait OpticalReplyExt {
    fn value(self) -> Value;
    fn unavailable(command: OpticalCommand, ready: bool, error: io::Error) -> Value;
}
impl OpticalReplyExt for OpticalReply {
    fn value(self) -> Value {
        serde_json::to_value(self).expect("optical reply is serializable")
    }
    fn unavailable(command: OpticalCommand, ready: bool, error: io::Error) -> Value {
        Self {
            settings: None,
            pon_mac: None,
            schema_version: 1,
            status: ReplyStatus::Error,
            action: command.action(),
            desired_enabled: None,
            tx_enabled: None,
            actor_state: OpticalState::Unknown,
            hardware_ready: ready,
            discovery_active: false,
            cleanup_pending: false,
            error: Some(OpticalError {
                code: format!("{:?}", error.kind()),
                message: error.to_string(),
            }),
        }
        .value()
    }
}
pub(crate) struct OpticalRequest {
    pub command: OpticalCommand,
    pub reply: SyncSender<Value>,
    pub deadline: Instant,
}
#[derive(Clone)]
pub(crate) struct OpticalClient {
    pub platform: PlatformContext,
    pub requests: SyncSender<OpticalRequest>,
    pub hardware_ready: Arc<AtomicBool>,
    pub stopping: Arc<AtomicBool>,
}
impl OpticalClient {
    pub(crate) fn request(&self, command: OpticalCommand) -> io::Result<Value> {
        if matches!(command, OpticalCommand::Shutdown) {
            // The fsl loop observes `stopping` and exits (draining the queue
            // with a bounded wait afterwards), so the request must already be
            // in the queue at that moment: enqueue first, then flip the flag
            // only after the send succeeded. A failed send leaves the flag
            // untouched and the loop keeps serving requests; the caller sees
            // the send error and a process stop still converges through the
            // termination path's own Shutdown request.
            let sent = self.enqueue(command);
            if sent.is_ok() {
                self.stopping.store(true, Ordering::Release);
            }
            return self.await_reply(sent);
        }
        if self.stopping.load(Ordering::Acquire) && !matches!(command, OpticalCommand::Status) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "controller is stopping",
            ));
        }
        if !self.hardware_ready.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "hardware initialization incomplete",
            ));
        }
        self.await_reply(self.enqueue(command))
    }

    fn enqueue(&self, command: OpticalCommand) -> io::Result<mpsc::Receiver<Value>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.requests
            .try_send(OpticalRequest {
                command,
                reply,
                deadline: Instant::now() + Duration::from_secs(12),
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "optical request queue full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::NotConnected, "optical owner unavailable")
                }
            })?;
        Ok(response)
    }

    fn await_reply(&self, response: io::Result<mpsc::Receiver<Value>>) -> io::Result<Value> {
        response?
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{error}; optical operation outcome unknown; query status"),
                )
            })
    }
}
/// Owner policy hooks, distinct from the platform transport/resource contracts.
pub(crate) trait OpticalBackend {
    fn validate_calibration(&self) -> io::Result<()> {
        Ok(())
    }
    /// Platform-specific programming after quiescing and disabling transmission.
    /// HM2004 restores its calibration as part of set_tx; FSL needs a separate recipe.
    fn apply_calibration(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn calibration_requires_disabled_tx(&self) -> bool {
        false
    }
    fn save_settings(&mut self, _settings: fttr_api::OpticalSettings) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "optical settings are not writable",
        ))
    }
    fn mac_tx_allowed(&self) -> bool {
        true
    }
    /// Apply and verify the subordinate MAC settings before acknowledging a request.
    /// A master switch that is already enabled must not skip a TX/FEC-only change.
    fn reconcile_controls(&mut self, _master_enabled: bool) -> io::Result<()> {
        Ok(())
    }
    fn ready(&self) -> bool;
    fn cleanup_pending(&self) -> bool;
    fn quiesce(&mut self) -> io::Result<()>;
    fn set_tx(&mut self, enabled: bool) -> io::Result<()>;
    fn read_tx(&mut self) -> io::Result<bool>;
    fn desired(&self) -> io::Result<bool>;
    fn save(&mut self, enabled: bool) -> io::Result<()>;
}
pub(crate) struct OpticalActor {
    pub state: OpticalState,
}
impl OpticalActor {
    /// Reuse request failure policy for autonomous TX/FEC reconciliation too.
    pub(crate) fn reconcile(&mut self, backend: &mut impl OpticalBackend) -> io::Result<()> {
        backend
            .reconcile_controls(self.state == OpticalState::Enabled)
            .map_err(|error| self.fail_closed(backend, &mut None, error, false))
    }

    fn fail_closed(
        &mut self,
        backend: &mut impl OpticalBackend,
        tx_enabled: &mut Option<bool>,
        error: io::Error,
        quiesced: bool,
    ) -> io::Error {
        self.state = OpticalState::Unknown;
        let mut errors = vec![error.to_string()];
        if backend.ready() {
            // Neither cleanup failure nor a status read failure may skip the off write.
            if let Err(error) = backend.set_tx(false) {
                errors.push(format!("recovery reset failed: {error}"));
            }
            match backend.read_tx() {
                Ok(actual) => {
                    *tx_enabled = Some(actual);
                    if actual {
                        errors.push("recovery TX readback remains enabled".into());
                    }
                }
                Err(error) => {
                    *tx_enabled = None;
                    errors.push(format!("recovery readback failed: {error}"));
                }
            }
            if !quiesced {
                if let Err(error) = backend.quiesce() {
                    errors.push(format!("quiesce failed: {error}"));
                }
            }
        }
        io::Error::new(error.kind(), errors.join("; "))
    }

    pub(crate) fn dispatch(
        &mut self,
        request: &OpticalRequest,
        backend: &mut impl OpticalBackend,
        stopping: &AtomicBool,
    ) -> Value {
        if Instant::now() > request.deadline {
            OpticalReply::unavailable(
                request.command,
                backend.ready(),
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "optical request expired before execution",
                ),
            )
        } else if stopping.load(Ordering::Acquire)
            && !matches!(
                request.command,
                OpticalCommand::Shutdown | OpticalCommand::Status
            )
        {
            OpticalReply::unavailable(
                request.command,
                backend.ready(),
                io::Error::new(io::ErrorKind::NotConnected, "controller is stopping"),
            )
        } else {
            self.handle(request.command, backend)
        }
    }
    pub(crate) fn handle(
        &mut self,
        command: OpticalCommand,
        backend: &mut impl OpticalBackend,
    ) -> Value {
        let mut tx_enabled = None;
        let mut quiesce_attempted = false;
        let desired = backend.desired();
        let mut desired_enabled = desired.as_ref().ok().copied();
        let result = if !backend.ready() && !matches!(command, OpticalCommand::Shutdown) {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "hardware initialization incomplete",
            ))
        } else {
            match command {
                OpticalCommand::Set(settings) => match backend.save_settings(settings) {
                    Ok(()) => return self.handle(OpticalCommand::Reload, backend),
                    Err(error) => {
                        let mut reply = self.handle(OpticalCommand::Status, backend);
                        reply["status"] = "error".into();
                        reply["error"] = serde_json::json!({"code":"Configuration", "message":error.to_string()});
                        return reply;
                    }
                },
                OpticalCommand::On
                | OpticalCommand::Off
                | OpticalCommand::Reload
                | OpticalCommand::Reinitialize
                | OpticalCommand::Shutdown => {
                    let target = match command {
                        OpticalCommand::On | OpticalCommand::Off => {
                            let enabled = matches!(command, OpticalCommand::On);
                            backend.save(enabled).and_then(|()| {
                                // Reload the committed settings before either platform
                                // applies subordinate MAC controls. The settings cache
                                // may still reflect the value read before save().
                                let actual = backend.desired()?;
                                desired_enabled = Some(actual);
                                if actual != enabled {
                                    return Err(io::Error::other(
                                        "saved optical setting did not match readback",
                                    ));
                                }
                                Ok(actual)
                            })
                        }
                        OpticalCommand::Shutdown => Ok(false),
                        OpticalCommand::Reinitialize => backend.validate_calibration().and(desired),
                        _ => desired,
                    };
                    // Persist first; failures must not falsely report a saved setting.
                    if let Err(error) = target {
                        let mut reply = if matches!(
                            command,
                            OpticalCommand::Reload | OpticalCommand::Reinitialize
                        ) {
                            self.handle(OpticalCommand::Shutdown, backend)
                        } else {
                            self.handle(OpticalCommand::Status, backend)
                        };
                        reply["status"] = "error".into();
                        reply["action"] = serde_json::to_value(command.action()).unwrap();
                        reply["error"] = serde_json::json!({"code":"Configuration", "message":error.to_string()});
                        return reply;
                    }
                    let enabled = target.unwrap();
                    if matches!(command, OpticalCommand::Reload)
                        && self.state
                            == if enabled {
                                OpticalState::Enabled
                            } else {
                                OpticalState::Disabled
                            }
                        && backend.read_tx().ok() == Some(enabled)
                    {
                        let mut reply = self.handle(OpticalCommand::Status, backend);
                        reply["action"] = serde_json::to_value(command.action()).unwrap();
                        return reply;
                    }
                    let was_enabled = self.state != OpticalState::Disabled;
                    self.state = OpticalState::Unknown;
                    let quiesced = if enabled
                        && !was_enabled
                        && !matches!(command, OpticalCommand::Reinitialize)
                    {
                        Ok(())
                    } else {
                        quiesce_attempted = true;
                        backend.quiesce()
                    };
                    // A failed quiesce must still permit reset, never a fresh enable.
                    let target_tx = enabled && quiesced.is_ok();
                    let preparation = if quiesced.is_err() {
                        Err(io::Error::other("optical users could not be quiesced"))
                    } else if matches!(command, OpticalCommand::Reinitialize) {
                        let disabled = if backend.calibration_requires_disabled_tx() {
                            backend.set_tx(false)
                        } else {
                            Ok(())
                        };
                        disabled.and_then(|()| backend.apply_calibration())
                    } else {
                        Ok(())
                    };
                    let transition = preparation
                        .and_then(|()| backend.set_tx(target_tx))
                        .and_then(|()| backend.reconcile_controls(target_tx))
                        .and_then(|()| {
                            let actual = backend.read_tx()?;
                            tx_enabled = Some(actual);
                            if actual == target_tx {
                                Ok(())
                            } else {
                                Err(io::Error::other("optical TX readback mismatch"))
                            }
                        });
                    let result = match (quiesced, transition) {
                        (Ok(()), result) => result,
                        (Err(error), Ok(())) => Err(error),
                        (Err(quiesce), Err(transition)) => Err(io::Error::other(format!(
                            "quiesce failed: {quiesce}; optical transition failed: {transition}"
                        ))),
                    };
                    result.map(|()| {
                        self.state = if enabled {
                            OpticalState::Enabled
                        } else {
                            OpticalState::Disabled
                        };
                    })
                }
                OpticalCommand::Status => backend.read_tx().and_then(|actual| {
                    tx_enabled = Some(actual);
                    if self.state == OpticalState::Unknown
                        || (self.state == OpticalState::Disabled && actual)
                    {
                        // fail_closed recovery has already run before this reply
                        // reaches the client; a failed recovery appends its own
                        // detail to this message, so the base wording stays
                        // neutral about the outcome.
                        return Err(io::Error::other(
                            "optical state is unconfirmed; transmission recovery attempted; explicit reload required",
                        ));
                    }
                    // The trait does not guarantee reconcile_controls quiesces
                    // (platform adapters do via the shared MAC policy), so the
                    // downgrade still owes its own explicit quiesce.
                    backend.reconcile_controls(actual)?;
                    if self.state == OpticalState::Enabled && !actual {
                        self.state = OpticalState::Disabled;
                        quiesce_attempted = true;
                        backend.quiesce()?;
                    }
                    desired.map(|_| ())
                }),
            }
        };
        let result = match result {
            Err(error) => Err(self.fail_closed(backend, &mut tx_enabled, error, quiesce_attempted)),
            result => result,
        };
        OpticalReply {
            settings: Some(crate::optical::settings()),
            pon_mac: Some(super::pon_mac::snapshot()),
            schema_version: 1,
            status: if result.is_ok() {
                ReplyStatus::Success
            } else {
                ReplyStatus::Error
            },
            action: command.action(),
            desired_enabled,
            tx_enabled,
            actor_state: self.state,
            hardware_ready: backend.ready(),
            discovery_active: backend.ready()
                && backend.mac_tx_allowed()
                && self.state == OpticalState::Enabled
                && !backend.cleanup_pending(),
            cleanup_pending: backend.cleanup_pending(),
            error: result.err().map(|error| OpticalError {
                code: format!("{:?}", error.kind()),
                message: error.to_string(),
            }),
        }
        .value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn on_and_off_refresh_committed_settings_before_reconciling_mac() {
        struct CachedBackend {
            stored: bool,
            cached: Cell<bool>,
            master: bool,
            mac: bool,
        }
        impl OpticalBackend for CachedBackend {
            fn ready(&self) -> bool {
                true
            }
            fn cleanup_pending(&self) -> bool {
                false
            }
            fn quiesce(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn desired(&self) -> io::Result<bool> {
                self.cached.set(self.stored);
                Ok(self.stored)
            }
            fn save(&mut self, enabled: bool) -> io::Result<()> {
                self.stored = enabled;
                Ok(())
            }
            fn set_tx(&mut self, enabled: bool) -> io::Result<()> {
                self.master = enabled;
                Ok(())
            }
            fn read_tx(&mut self) -> io::Result<bool> {
                Ok(self.master)
            }
            fn reconcile_controls(&mut self, master: bool) -> io::Result<()> {
                self.mac = master && self.cached.get();
                Ok(())
            }
        }
        let mut backend = CachedBackend {
            stored: false,
            cached: Cell::new(false),
            master: false,
            mac: false,
        };
        let mut actor = OpticalActor {
            state: OpticalState::Disabled,
        };
        for (command, expected) in [(OpticalCommand::On, true), (OpticalCommand::Off, false)] {
            let reply = actor.handle(command, &mut backend);
            assert_eq!(reply["status"], "success");
            assert_eq!(reply["desired_enabled"], expected);
            assert_eq!(backend.master, expected);
            assert_eq!(
                backend.mac, expected,
                "MAC must use the freshly saved setting"
            );
        }
    }

    #[test]
    fn shutdown_enqueues_before_requesting_the_loop_to_stop() {
        let (requests, receiver) = mpsc::sync_channel(2);
        let stopping = Arc::new(AtomicBool::new(false));
        let client = OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests,
            hardware_ready: Arc::new(AtomicBool::new(true)),
            stopping: stopping.clone(),
        };
        // Once the flip is visible to this observer (Release store ↔ Acquire
        // load), the enqueue that preceded it in program order must already be
        // in the queue — this is what discriminates the enqueue-before-flip
        // ordering instead of passing under either order.
        let observer_stopping = stopping.clone();
        let observer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !observer_stopping.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "flag never set");
                std::thread::sleep(Duration::from_millis(1));
            }
            let request = receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("shutdown request enqueued before the flag flip");
            let _ = request.reply.send(serde_json::json!({"status": "success"}));
        });
        let reply = client.request(OpticalCommand::Shutdown).expect("reply");
        assert_eq!(reply["status"], "success");
        observer.join().expect("observer thread");
        assert!(
            stopping.load(Ordering::Acquire),
            "stopping is set once the send succeeded"
        );
    }

    #[test]
    fn shutdown_leaves_stopping_untouched_when_the_queue_is_full() {
        let (requests, receiver) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let client = OpticalClient {
            platform: crate::platform::context::PlatformContext::test_context(),
            requests,
            hardware_ready: Arc::new(AtomicBool::new(true)),
            stopping: stopping.clone(),
        };
        // Keep the receiver alive but never drain it: the shutdown enqueue must
        // fail with WouldBlock and, per the request-path comment, the flag flip
        // must not happen — the loop stays free to serve later requests.
        let _pending = client
            .enqueue(OpticalCommand::Status)
            .expect("fill the queue");
        let error = client
            .request(OpticalCommand::Shutdown)
            .expect_err("queue full");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(
            !stopping.load(Ordering::Acquire),
            "a failed send must leave the flag untouched"
        );
        drop(receiver);
    }

    struct Backend {
        master: bool,
        settings: fttr_api::OpticalSettings,
        mac: (bool, bool),
        resets: usize,
        fail: bool,
        programs: usize,
        program_fails: bool,
        quiesce_fails: bool,
        ignore_disable: bool,
    }
    impl OpticalBackend for Backend {
        fn calibration_requires_disabled_tx(&self) -> bool {
            true
        }
        fn apply_calibration(&mut self) -> io::Result<()> {
            assert!(
                !self.master,
                "programming must happen with transmission disabled"
            );
            self.programs += 1;
            if self.program_fails {
                Err(io::Error::other("I2C write failed"))
            } else {
                Ok(())
            }
        }
        fn ready(&self) -> bool {
            true
        }
        fn cleanup_pending(&self) -> bool {
            false
        }
        fn quiesce(&mut self) -> io::Result<()> {
            if self.quiesce_fails {
                Err(io::Error::other("deactivation failed"))
            } else {
                Ok(())
            }
        }
        fn set_tx(&mut self, enabled: bool) -> io::Result<()> {
            self.resets += 1;
            if enabled || !self.ignore_disable {
                self.master = enabled;
            }
            Ok(())
        }
        fn read_tx(&mut self) -> io::Result<bool> {
            Ok(self.master)
        }
        fn desired(&self) -> io::Result<bool> {
            Ok(self.settings.enabled)
        }
        fn save(&mut self, enabled: bool) -> io::Result<()> {
            self.settings.enabled = enabled;
            Ok(())
        }
        fn save_settings(&mut self, settings: fttr_api::OpticalSettings) -> io::Result<()> {
            self.settings = settings;
            Ok(())
        }
        fn reconcile_controls(&mut self, master: bool) -> io::Result<()> {
            if self.fail {
                return Err(io::Error::other("FEC write failed"));
            }
            self.mac = (
                master && self.settings.enabled && self.settings.downstream_tx_enabled,
                master && self.settings.enabled && self.settings.fec_enabled,
            );
            Ok(())
        }
    }
    #[test]
    fn subordinate_changes_are_acknowledged_only_after_application_without_master_reset() {
        let settings = fttr_api::OpticalSettings {
            enabled: true,
            downstream_tx_enabled: true,
            fec_enabled: false,
        };
        let mut backend = Backend {
            master: true,
            settings,
            mac: (true, false),
            resets: 0,
            fail: false,
            programs: 0,
            program_fails: false,
            quiesce_fails: false,
            ignore_disable: false,
        };
        let mut actor = OpticalActor {
            state: OpticalState::Enabled,
        };
        for (tx, fec) in [(true, true), (false, true), (true, false)] {
            let response = actor.handle(
                OpticalCommand::Set(fttr_api::OpticalSettings {
                    downstream_tx_enabled: tx,
                    fec_enabled: fec,
                    ..settings
                }),
                &mut backend,
            );
            assert_eq!(response["status"], "success");
            assert_eq!(backend.mac, (tx, fec));
            assert_eq!(backend.resets, 0);
            assert!(actor.state == OpticalState::Enabled);
        }
        actor.handle(OpticalCommand::Shutdown, &mut backend);
        assert!(!backend.master);
        assert_eq!(backend.mac, (false, false));
        assert!(
            backend.settings.enabled,
            "shutdown must not overwrite saved startup configuration"
        );
        backend.master = true;
        actor.state = OpticalState::Enabled;
        backend.fail = true;
        let response = actor.handle(OpticalCommand::Reload, &mut backend);
        assert_eq!(response["status"], "error");
        assert!(actor.state == OpticalState::Unknown);
    }

    #[test]
    fn reinitialize_programs_while_disabled_and_failure_never_restores_transmission() {
        for enabled in [true, false] {
            let mut backend = Backend {
                master: enabled,
                settings: fttr_api::OpticalSettings {
                    enabled,
                    downstream_tx_enabled: true,
                    fec_enabled: false,
                },
                mac: (enabled, false),
                resets: 0,
                fail: false,
                programs: 0,
                program_fails: false,
                quiesce_fails: false,
                ignore_disable: false,
            };
            let mut actor = OpticalActor {
                state: if enabled {
                    OpticalState::Enabled
                } else {
                    OpticalState::Disabled
                },
            };
            let reply = actor.handle(OpticalCommand::Reinitialize, &mut backend);
            assert_eq!(reply["status"], "success");
            assert_eq!(backend.programs, 1);
            assert_eq!(backend.master, enabled);
            assert_eq!(backend.settings.enabled, enabled);
            backend.program_fails = true;
            let reply = actor.handle(OpticalCommand::Reinitialize, &mut backend);
            assert_eq!(reply["status"], "error");
            assert!(!backend.master);
            assert_eq!(backend.settings.enabled, enabled);
            assert!(actor.state == OpticalState::Unknown);
            backend.program_fails = false;
            backend.quiesce_fails = true;
            let count = backend.programs;
            let reply = actor.handle(OpticalCommand::Reinitialize, &mut backend);
            assert_eq!(reply["status"], "error");
            assert_eq!(
                backend.programs, count,
                "no programming after failed quiesce"
            );
            assert!(!backend.master);
        }
    }

    #[test]
    fn status_and_periodic_failures_close_tx_and_unknown_status_never_reenables() {
        let mut backend = Backend {
            master: true,
            settings: fttr_api::OpticalSettings {
                enabled: true,
                downstream_tx_enabled: true,
                fec_enabled: false,
            },
            mac: (true, false),
            resets: 0,
            fail: true,
            programs: 0,
            program_fails: false,
            quiesce_fails: false,
            ignore_disable: false,
        };
        let mut actor = OpticalActor {
            state: OpticalState::Enabled,
        };
        let reply = actor.handle(OpticalCommand::Status, &mut backend);
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["tx_enabled"], false);
        assert!(!backend.master);
        backend.master = true;
        actor.state = OpticalState::Enabled;
        assert!(actor.reconcile(&mut backend).is_err());
        assert!(!backend.master);
        assert!(actor.state == OpticalState::Unknown);
        backend.fail = false;
        backend.master = true;
        let reply = actor.handle(OpticalCommand::Status, &mut backend);
        assert_eq!(reply["status"], "error");
        assert!(!backend.master, "unknown state requires an explicit reload");
        backend.fail = true;
        backend.master = true;
        backend.ignore_disable = true;
        let reply = actor.handle(OpticalCommand::Status, &mut backend);
        assert_eq!(reply["tx_enabled"], true);
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("readback remains enabled"));
        assert!(
            backend.settings.enabled,
            "failures must preserve saved settings"
        );
    }
}
