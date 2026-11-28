//! Per-registration workers. Cancellation precedes teardown; tokens are never reused.
use crate::model::{BusinessPhase, BusinessStatus};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Default)]
pub(crate) struct State {
    pub generation: u64,
    pub phase: BusinessPhase,
    pub error: Option<String>,
    pub identity: Option<crate::model::OnuIdentity>,
    pub identity_at: Option<std::time::Instant>,
    pub mib: Option<std::sync::mpsc::SyncSender<super::inventory::WorkerRequest>>,
    pub wake: Option<Arc<crate::platform::openwrt::wake::Wake>>,
}
impl State {
    pub fn business_status(&self, carrier: bool, management_ready: bool) -> BusinessStatus {
        let phase = match self.phase {
            BusinessPhase::WanBootstrapPending => match (management_ready, carrier) {
                (true, true) => BusinessPhase::Verified,
                (true, false) => BusinessPhase::Verifying,
                (false, _) => BusinessPhase::WanBootstrapPending,
            },
            BusinessPhase::Verifying => {
                if carrier {
                    BusinessPhase::Verified
                } else {
                    BusinessPhase::Verifying
                }
            }
            BusinessPhase::NotStarted
            | BusinessPhase::CollectingIdentity
            | BusinessPhase::RetryingIdentity
            | BusinessPhase::Unsupported
            | BusinessPhase::Configuring
            | BusinessPhase::Verified
            | BusinessPhase::Retrying
            | BusinessPhase::Offline => self.phase,
        };
        BusinessStatus {
            phase,
            generation: self.generation,
            error: self.error.clone(),
        }
    }
}
pub(crate) type States = Arc<Mutex<BTreeMap<u8, State>>>;

// Selection policy can change while an ONU remains registered (config reload).
// Keep unsupported workers cancellable and re-evaluate without resetting PLOAM
// or restarting healthy ONUs. No OMCI writes occur while waiting for permission.
fn wait_for_adapter(
    cancellation: &AtomicBool,
    mut select: impl FnMut() -> crate::devices::Adapter,
    mut unsupported: impl FnMut(),
) -> Option<crate::devices::Adapter> {
    while !cancellation.load(Ordering::Acquire) {
        let adapter = select();
        if cancellation.load(Ordering::Acquire) {
            return None;
        }
        if adapter.capabilities().business {
            return Some(adapter);
        }
        unsupported();
        for _ in 0..10 {
            if cancellation.load(Ordering::Acquire) {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

pub(crate) struct Services {
    platform: crate::platform::context::PlatformContext,
    pub states: States,
    tokens: BTreeMap<u8, Arc<AtomicBool>>,
}
impl Services {
    pub fn new(platform: crate::platform::context::PlatformContext, states: States) -> Self {
        Self {
            platform,
            states,
            tokens: BTreeMap::new(),
        }
    }
    pub fn stop(&mut self, id: u8) {
        crate::omci_auth::forget(id);
        if let Some(token) = self.tokens.remove(&id) {
            token.store(true, Ordering::Release);
        }
        if let Some(state) = self.states.lock().unwrap().get_mut(&id) {
            state.phase = BusinessPhase::Offline;
            state.mib = None;
            if let Some(wake) = state.wake.take() {
                wake.notify();
            }
        }
    }
    /// Cancel workers even when the hardware ONU inventory cannot be read.
    pub fn stop_all(&mut self) {
        let ids: Vec<_> = self.tokens.keys().copied().collect();
        for id in ids {
            self.stop(id);
        }
    }
    pub fn refresh_auth(&mut self) {
        let stale: Vec<_> = self
            .tokens
            .iter()
            .filter(|(_, token)| token.load(Ordering::Acquire))
            .map(|(&id, _)| id)
            .collect();
        for id in stale {
            if let Some(serial) = onu_serial(id) {
                self.start(id, serial[..4].to_owned());
            }
        }
    }
    pub fn start(&mut self, id: u8, vendor: String) {
        self.stop(id);
        let generation = self
            .states
            .lock()
            .unwrap()
            .get(&id)
            .map_or(1, |state| state.generation + 1);
        let (mib, requests) = std::sync::mpsc::sync_channel(1);
        self.states.lock().unwrap().insert(
            id,
            State {
                generation,
                phase: BusinessPhase::CollectingIdentity,
                mib: Some(mib),
                ..State::default()
            },
        );
        let token = Arc::new(AtomicBool::new(false));
        self.tokens.insert(id, token.clone());
        let states = self.states.clone();
        let platform = self.platform;
        let _ = thread::Builder::new()
            .name(format!("onu-{id}-svc"))
            .stack_size(256 * 1024)
            .spawn(move || {
            // A transient equipment-ID GET failure must not permanently classify
            // an otherwise supported device as unsupported for this generation.
            let identity = loop {
                let identity = super::inventory::collect(platform, id, token.clone()).unwrap_or_else(|error| crate::model::OnuIdentity {
                    errors: vec![error.to_string()], ..Default::default()
                });
                if token.load(Ordering::Acquire) { return; }
                if identity.equipment_id.is_some() { break identity; }
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.identity = Some(identity);
                    state.identity_at = Some(std::time::Instant::now());
                    state.phase = BusinessPhase::RetryingIdentity;
                }
                for _ in 0..100 {
                    if token.load(Ordering::Acquire) { return; }
                    thread::sleep(Duration::from_millis(100));
                }
            };
            if token.load(Ordering::Acquire) { return; }
            if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                state.identity = Some(identity.clone());
                state.identity_at = Some(std::time::Instant::now());
            }
            let mut reported_unsupported = false;
            struct CancelOnDrop(Arc<AtomicBool>);
            impl Drop for CancelOnDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let unsupported_telemetry = CancelOnDrop(Arc::new(AtomicBool::new(false)));
            let Some(adapter) = wait_for_adapter(&token, || {
                let serial = super::service::onu_serial(id);
                crate::devices::select_with_serial(&vendor, &identity, serial.as_deref())
            }, || {
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.phase = BusinessPhase::Unsupported;
                }
                if !reported_unsupported {
                    println!("[!] ONU {id} generation={generation} vendor={vendor}: identity collected; business profile unsupported; waiting for adapter policy change");
                    super::telemetry::start(platform, id, unsupported_telemetry.0.clone());
                    reported_unsupported = true;
                }
            }) else { return; };
            drop(unsupported_telemetry);
            let set = |phase: BusinessPhase, error: Option<String>| {
                if token.load(Ordering::Acquire) { return; }
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.phase = phase; state.error = error;
                }
            };
            let mut wake = None;
            let mut auth_failures = 0;
            while !token.load(Ordering::Acquire) {
                set(BusinessPhase::Configuring, None);
                let result = (|| {
                    // Notification allocation failures use the normal supervisor retry.
                    if wake.is_none() {
                        let notification = Arc::new(crate::platform::openwrt::wake::Wake::new()?);
                        let mut states = states.lock().unwrap();
                        if token.load(Ordering::Acquire) { return Ok(()); }
                        if let Some(state) = states.get_mut(&id).filter(|state| state.generation == generation) {
                            state.wake = Some(notification.clone());
                        }
                        wake = Some(notification);
                    }
                    adapter.configure(platform, id, token.clone())
                    .and_then(|()| {
                        if token.load(Ordering::Acquire) { return Ok(()); }
                        let phase = adapter.verification_status();
                        println!("[+] ONU {id} generation={generation}: OMCI profile readback complete; {phase}; starting evidence monitor");
                        set(phase, None);
                        super::health::watch(platform, id, token.clone(), vendor.as_bytes().try_into().map_err(|_| std::io::Error::other("invalid vendor ID"))?, &requests, wake.as_ref().unwrap().clone())
                    })
                })();
                if token.load(Ordering::Acquire) { break; }
                if let Err(error) = result {
                    eprintln!("[-] ONU {id} generation={generation} service retry: {error}");
                    set(BusinessPhase::Retrying, Some(error.to_string()));
                    if crate::omci_auth::exchange_failed(id) { auth_failures += 1; }
                    if error.kind() == std::io::ErrorKind::PermissionDenied || auth_failures >= 3 {
                        // A mismatch is stable for this registration and policy.
                        // Keep the worker cancellable; reload creates a new generation.
                        while !token.load(Ordering::Acquire) { thread::sleep(Duration::from_millis(100)); }
                        break;
                    }
                }
                for _ in 0..100 {
                    if token.load(Ordering::Acquire) { break; }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            println!("[*] ONU {id} generation={generation} worker resources released");
        });
    }
}
static PON_ENGINE: std::sync::OnceLock<Arc<Mutex<crate::pon_engine::PonEngine>>> =
    std::sync::OnceLock::new();

pub(crate) fn init_pon_engine(engine: Arc<Mutex<crate::pon_engine::PonEngine>>) {
    let _ = PON_ENGINE.set(engine);
}

pub(crate) fn operation_confirmed(id: u8) -> bool {
    if let Some(engine) = PON_ENGINE.get() {
        if let Ok(guard) = engine.lock() {
            return guard.active_onus.iter().any(|onu| {
                onu.onu_id == id && onu.state == crate::pon_engine::ploam::OnuState::O5Operation
            });
        }
    }
    false
}

pub(crate) fn onu_serial(id: u8) -> Option<String> {
    if let Some(engine) = PON_ENGINE.get() {
        if let Ok(guard) = engine.lock() {
            return guard
                .active_onus
                .iter()
                .find(|onu| onu.onu_id == id)
                .map(|onu| onu.full_sn_str());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::State;
    use crate::model::{BusinessPhase, BusinessStatus};

    #[test]
    fn unsupported_worker_observes_policy_change_without_reregistration() {
        use crate::devices::Adapter;
        use std::cell::Cell;
        use std::sync::atomic::AtomicBool;

        let cancellation = AtomicBool::new(false);
        let allowed = Cell::new(false);
        let selections = Cell::new(0);
        let adapter = super::wait_for_adapter(
            &cancellation,
            || {
                selections.set(selections.get() + 1);
                if allowed.get() {
                    Adapter::GenericBridge
                } else {
                    Adapter::Unavailable
                }
            },
            || allowed.set(true), // Reload enables the previously denied bridge.
        );
        assert_eq!(adapter, Some(Adapter::GenericBridge));
        assert_eq!(selections.get(), 2);
    }

    #[test]
    fn unsupported_worker_cancellation_prevents_later_configuration() {
        use crate::devices::Adapter;
        use std::sync::atomic::{AtomicBool, Ordering};

        let cancellation = AtomicBool::new(false);
        assert_eq!(
            super::wait_for_adapter(
                &cancellation,
                || Adapter::Unavailable,
                || cancellation.store(true, Ordering::Release),
            ),
            None
        );
        assert_eq!(
            super::wait_for_adapter(
                &cancellation,
                || panic!("a stopped registration must not select an adapter"),
                || unreachable!(),
            ),
            None
        );
    }

    #[test]
    fn supported_worker_does_not_wait_for_policy_change() {
        use crate::devices::Adapter;
        assert_eq!(
            super::wait_for_adapter(
                &std::sync::atomic::AtomicBool::new(false),
                || Adapter::GenericBridge,
                || panic!("healthy devices must not enter unsupported state"),
            ),
            Some(Adapter::GenericBridge)
        );
    }

    #[test]
    fn business_status_tracks_management_and_carrier_readiness() {
        // Given: a configured V152 waiting for management bootstrap.
        let state = State {
            phase: BusinessPhase::WanBootstrapPending,
            generation: 1,
            ..State::default()
        };
        for (carrier, management_ready, expected) in [
            (true, true, BusinessPhase::Verified),
            (false, true, BusinessPhase::Verifying),
            (true, false, BusinessPhase::WanBootstrapPending),
            (false, false, BusinessPhase::WanBootstrapPending),
        ] {
            // When: status is requested with current readiness evidence.
            let status = state.business_status(carrier, management_ready);
            // Then: neither CWMP nor carrier alone certifies V152 business readiness.
            assert_eq!(
                status,
                BusinessStatus {
                    phase: expected,
                    generation: 1,
                    error: None
                },
                "carrier={carrier}, management_ready={management_ready}"
            );
        }
    }

    #[test]
    fn business_status_preserves_other_worker_phases_and_errors() {
        // Given: worker states outside the V152 bootstrap gate.
        for (phase, carrier, error, expected) in [
            (
                BusinessPhase::Verifying,
                true,
                None,
                BusinessPhase::Verified,
            ),
            (
                BusinessPhase::Verifying,
                false,
                None,
                BusinessPhase::Verifying,
            ),
            (
                BusinessPhase::Configuring,
                true,
                None,
                BusinessPhase::Configuring,
            ),
            (
                BusinessPhase::Retrying,
                true,
                Some("readback failed"),
                BusinessPhase::Retrying,
            ),
            (BusinessPhase::Offline, true, None, BusinessPhase::Offline),
            (
                BusinessPhase::NotStarted,
                true,
                None,
                BusinessPhase::NotStarted,
            ),
            (
                BusinessPhase::CollectingIdentity,
                true,
                None,
                BusinessPhase::CollectingIdentity,
            ),
            (
                BusinessPhase::RetryingIdentity,
                true,
                None,
                BusinessPhase::RetryingIdentity,
            ),
            (
                BusinessPhase::Unsupported,
                true,
                None,
                BusinessPhase::Unsupported,
            ),
            (
                BusinessPhase::Verified,
                false,
                None,
                BusinessPhase::Verified,
            ),
        ] {
            let state = State {
                phase,
                generation: 2,
                error: error.map(str::to_owned),
                ..State::default()
            };
            // When: current CWMP readiness is available.
            let status = state.business_status(carrier, true);
            // Then: readiness does not hide worker transitions or errors.
            assert_eq!(
                status,
                BusinessStatus {
                    phase: expected,
                    generation: 2,
                    error: error.map(str::to_owned)
                }
            );
        }
    }

    #[test]
    fn stopping_worker_preserves_generation_and_error() {
        let wake = std::sync::Arc::new(crate::platform::openwrt::wake::Wake::new().unwrap());
        let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (sender, _requests) = std::sync::mpsc::sync_channel(1);
        let states =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::from([(
                1,
                State {
                    phase: BusinessPhase::Retrying,
                    generation: 7,
                    error: Some("readback failed".into()),
                    wake: Some(wake.clone()),
                    mib: Some(sender),
                    ..State::default()
                },
            )])));
        let mut services = super::Services::new(
            crate::platform::context::PlatformContext::test_context(),
            states.clone(),
        );
        services.tokens.insert(1, token.clone());
        services.stop(1);
        // A stopped worker wakes even with no packet traffic, and cannot accept new MIB work.
        wake.wait(
            &[],
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert!(token.load(std::sync::atomic::Ordering::SeqCst));
        assert!(states.lock().unwrap()[&1].wake.is_none());
        assert!(states.lock().unwrap()[&1].mib.is_none());
        let status = states.lock().unwrap()[&1].business_status(true, true);
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "phase": "offline", "generation": 7, "error": "readback failed"
            })
        );
    }

    #[test]
    fn absent_worker_serializes_as_not_started_without_error() {
        let status = State::default().business_status(true, true);
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "phase": "not_started", "generation": 0, "error": null
            })
        );
    }

    #[test]
    fn stop_all_cancels_every_worker_without_a_hardware_inventory() {
        let mut services = super::Services::new(
            crate::platform::context::PlatformContext::test_context(),
            Default::default(),
        );
        let tokens: Vec<_> = (0..3)
            .map(|id| {
                let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                services.tokens.insert(id, token.clone());
                token
            })
            .collect();
        services.stop_all();
        assert!(tokens
            .iter()
            .all(|token| token.load(std::sync::atomic::Ordering::Acquire)));
        assert!(services.tokens.is_empty());
        services.stop_all();
    }
}
