//! Credential authorization, independent of optical identity and business evidence.
use crate::platform::context::PlatformContext;
use crate::pon_engine::omci::OmciSession;
use std::{
    collections::BTreeMap,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

#[derive(Clone, Default, PartialEq, Eq)]
pub enum Policy {
    #[default]
    Disabled,
    LoidPassword {
        loid: Vec<u8>,
        password: Vec<u8>,
    },
}
// No Debug/Serialize: comparison credentials stay out of configuration diagnostics.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub global: Policy,
    pub devices: BTreeMap<String, Policy>,
    pub selections: BTreeMap<String, String>,
}
impl Config {
    pub fn effective(&self, serial: &str) -> &Policy {
        self.devices.get(serial).unwrap_or(&self.global)
    }
}

pub(crate) fn parse_policy(
    mode: &str,
    loid: &str,
    password: &str,
    device: bool,
) -> Result<Option<Policy>, String> {
    match mode {
        "" | "inherit" if device => Ok(None),
        "disabled" => Ok(Some(Policy::Disabled)),
        "loid_password" => {
            let valid = |v: &str, max: usize| {
                !v.is_empty() && v.len() <= max && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
            };
            if !valid(loid, 24) || !valid(password, 12) {
                return Err("OMCI authentication requires printable ASCII LOID (1–24 bytes) and password (1–12 bytes)".into());
            }
            Ok(Some(Policy::LoidPassword {
                loid: loid.as_bytes().to_vec(),
                password: password.as_bytes().to_vec(),
            }))
        }
        _ => Err("invalid OMCI auth_mode".into()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    pub name: &'static str,
    pub class: u16,
    pub loid_mask: u16,
    pub password_mask: u16,
    pub status_mask: u16,
    pub loid_width: usize,
    pub password_width: usize,
    pub recover_alloc_channel: bool,
}
pub(crate) const STANDARD: Layout = Layout {
    name: "onu-g",
    class: 256,
    loid_mask: 0x0040,
    password_mask: 0x0020,
    status_mask: 0x0010,
    loid_width: 24,
    password_width: 12,
    recover_alloc_channel: false,
};

struct Grant {
    serial: String,
    policy: Policy,
    backend: Layout,
    token: Arc<AtomicBool>,
    platform: PlatformContext,
    state: fttr_api::OmciAuthState,
    reported: Option<(String, String)>,
    epoch: u64,
}
#[derive(Default)]
struct Registry {
    config: Option<Config>,
    grants: BTreeMap<u8, Grant>,
    bindings: BTreeMap<String, String>,
    managed: bool,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Invalidate before publishing policy. The platform revokes both software and
/// offloaded business forwarding; OMCC stays available for reads and authentication.
pub(crate) fn configure(config: &Config) -> Result<(), String> {
    let mut r = registry().lock().unwrap();
    configure_registry(&mut r, config, |id, platform| {
        platform.backend().revoke_business(id)
    })
}
fn configure_registry(
    r: &mut Registry,
    config: &Config,
    mut revoke: impl FnMut(u8, PlatformContext) -> io::Result<()>,
) -> Result<(), String> {
    let mut failed = false;
    let selections = r
        .config
        .as_ref()
        .map(|c| c.selections.clone())
        .unwrap_or_default();
    for (&id, grant) in &mut r.grants {
        if config.effective(&grant.serial) != &grant.policy
            || selections.get(&grant.serial) != config.selections.get(&grant.serial)
        {
            grant.state = fttr_api::OmciAuthState::Pending;
            grant.token.store(true, Ordering::Release);
            failed |= revoke(id, grant.platform).is_err();
        }
    }
    if failed {
        return Err("OMCI authentication forwarding revocation failed".into());
    }
    r.config = Some(config.clone());
    Ok(())
}

pub(crate) fn forget(id: u8) {
    if let Some(grant) = registry().lock().unwrap().grants.remove(&id) {
        grant.token.store(true, Ordering::Release);
        let _ = grant.platform.backend().revoke_business(id);
    }
}
fn permitted(g: &Grant) -> bool {
    !g.token.load(Ordering::Acquire)
        && matches!(
            g.state,
            fttr_api::OmciAuthState::Verified | fttr_api::OmciAuthState::Skipped
        )
}
pub(crate) fn allowed(serial: &str) -> bool {
    #[cfg(test)]
    if TEST_AUTHORIZED.with(|s| s.borrow().contains(serial)) {
        return true;
    }
    let r = registry().lock().unwrap();
    // With no daemon policy installed, standalone configuration tools remain read-only callers.
    let Some(config) = &r.config else {
        return false;
    };
    r.grants
        .values()
        .any(|g| g.serial == serial && &g.policy == config.effective(serial) && permitted(g))
}
/// Serialize carrier admission with policy invalidation, avoiding a check/set race.
pub(crate) fn admit(id: u8, action: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    let r = registry().lock().unwrap();
    if !r.grants.get(&id).is_some_and(permitted) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OMCI authentication pending",
        ));
    }
    action()
}
pub(crate) fn status(serial: &str) -> fttr_api::OmciAuthStatus {
    let r = registry().lock().unwrap();
    let policy = r.config.as_ref().map(|c| c.effective(serial));
    let mode = match policy {
        Some(Policy::LoidPassword { .. }) => fttr_api::OmciAuthMode::LoidPassword,
        _ => fttr_api::OmciAuthMode::Disabled,
    };
    let grant = r.grants.values().find(|g| {
        g.serial == serial && Some(&g.policy) == policy && !g.token.load(Ordering::Acquire)
    });
    fttr_api::OmciAuthStatus {
        mode,
        source: if r
            .config
            .as_ref()
            .is_some_and(|c| c.devices.contains_key(serial))
        {
            fttr_api::OmciAuthSource::Device
        } else {
            fttr_api::OmciAuthSource::Global
        },
        state: grant.map_or(fttr_api::OmciAuthState::Pending, |g| g.state),
        backend: grant.map(|g| g.backend.name.to_string()),
        reported_loid: grant.and_then(|g| g.reported.as_ref().map(|v| v.0.clone())),
        reported_password: grant.and_then(|g| g.reported.as_ref().map(|v| v.1.clone())),
    }
}

pub(crate) fn view(serial: &str) -> serde_json::Value {
    serde_json::to_value(status(serial)).expect("OmciAuthStatus serializes")
}

pub(crate) fn authenticate(
    platform: PlatformContext,
    id: u8,
    token: Arc<AtomicBool>,
    backend: Layout,
) -> io::Result<()> {
    let serial = crate::controller::service::onu_serial(id)
        .ok_or_else(|| io::Error::other("OMCI authentication identity unavailable"))?;
    let policy = {
        let mut r = registry().lock().unwrap();
        let policy = r
            .config
            .as_ref()
            .map(|c| c.effective(&serial).clone())
            .unwrap_or_default();
        if r.grants.get(&id).is_some_and(|g| {
            g.serial == serial
                && g.policy == policy
                && g.backend == backend
                && Arc::ptr_eq(&g.token, &token)
                && permitted(g)
        }) {
            return Ok(());
        }
        static NEXT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
        r.grants.insert(
            id,
            Grant {
                serial: serial.clone(),
                policy: policy.clone(),
                backend,
                token: token.clone(),
                platform,
                state: fttr_api::OmciAuthState::Checking,
                reported: None,
                epoch,
            },
        );
        policy
    };
    let mut reported = None;
    let result = (|| {
        // Also retire forwarding left by an earlier daemon/registration before
        // exchanging credentials. A new grant must never inherit carrier.
        platform.backend().revoke_business(id)?;
        let exchange = (|| {
            let _lock = crate::platform::openwrt::onu_lock::lock_onu(id)?;
            let mut session = OmciSession::new(platform.transport(id)?);
            session.cancel_when(token.clone());
            let expected_serial = serial.clone();
            session.guard_registration(move || {
                crate::controller::service::operation_confirmed(id)
                    && crate::controller::service::onu_serial(id).as_deref()
                        == Some(&expected_serial)
            });
            let _lease = platform.management_lease(id)?;
            let _channel = platform.channel(id, 0)?;
            if backend.recover_alloc_channel {
                if let Err(error) = session.get(256, 0, 0x8000) {
                    if error.kind() != io::ErrorKind::TimedOut {
                        return Err(error);
                    }
                    platform.exchange_channel(id, platform.resources().onu(id)?.gem_id)?;
                }
            }
            match &policy {
                Policy::Disabled => {
                    let (loid, password) = read_credentials(&mut session, backend)?;
                    reported = Some((credential_text(&loid), credential_text(&password)));
                    Ok(())
                }
                Policy::LoidPassword { loid, password } => {
                    verify(&mut session, backend, loid, password, &mut reported)
                }
            }
        })();
        // Credential discovery is informational when authentication is disabled.
        // Unsupported reads or unavailable OMCI channels cannot deny this policy.
        if policy == Policy::Disabled {
            Ok(())
        } else {
            exchange
        }
    })();
    let mut r = registry().lock().unwrap();
    if r.config
        .as_ref()
        .is_none_or(|c| c.effective(&serial) != &policy)
        || token.load(Ordering::Acquire)
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "OMCI authentication invalidated",
        ));
    }
    if let Some(grant) = r
        .grants
        .get_mut(&id)
        .filter(|g| Arc::ptr_eq(&g.token, &token))
    {
        grant.reported = reported;
        grant.state = match &result {
            Ok(()) if policy == Policy::Disabled => fttr_api::OmciAuthState::Skipped,
            Ok(()) => fttr_api::OmciAuthState::Verified,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                fttr_api::OmciAuthState::Denied
            }
            Err(_) => fttr_api::OmciAuthState::Error,
        };
    }
    // Transport errors can contain raw frames; return only a typed, credential-free reason.
    result.map_err(|e| {
        io::Error::new(
            e.kind(),
            if e.kind() == io::ErrorKind::PermissionDenied {
                "OMCI authentication denied"
            } else {
                "OMCI authentication exchange failed"
            },
        )
    })
}

fn matches_field(bytes: &[u8], expected: &[u8], width: usize) -> bool {
    let Some(bytes) = bytes.get(..width) else {
        return false;
    };
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(width);
    bytes[..end] == *expected && bytes[end..].iter().all(|b| *b == 0)
}
pub(crate) fn verify(
    session: &mut OmciSession,
    layout: Layout,
    loid: &[u8],
    password: &[u8],
    reported: &mut Option<(String, String)>,
) -> io::Result<()> {
    let (actual_loid, actual_password) = read_credentials(session, layout)?;
    *reported = Some((
        credential_text(&actual_loid),
        credential_text(&actual_password),
    ));
    if loid.len() > layout.loid_width || password.len() > layout.password_width {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credentials exceed authentication backend width",
        ));
    }
    let status = if !matches_field(&actual_loid, loid, layout.loid_width) {
        2
    } else if !matches_field(&actual_password, password, layout.password_width) {
        3
    } else {
        1
    };
    let notification = session.set(layout.class, 0, layout.status_mask, &[status]);
    if status != 1 {
        // Local denial is authoritative even when the ONU refuses failure notification.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OMCI credentials mismatch",
        ));
    }
    notification?;
    let response = session.get(layout.class, 0, layout.status_mask)?;
    if response.payload[3] != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OMCI credentials status readback failed",
        ));
    }
    Ok(())
}

fn credential_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

pub(crate) fn bind(devices: &[crate::provisioning::model::Device]) {
    registry().lock().unwrap().bindings = devices
        .iter()
        .map(|d| (d.cwmp_id.clone(), d.optical_sn.clone()))
        .collect();
}
pub(crate) fn install() {
    registry().lock().unwrap().managed = true;
}
pub(crate) fn exchange_failed(id: u8) -> bool {
    registry()
        .lock()
        .unwrap()
        .grants
        .get(&id)
        .is_some_and(|g| g.state == fttr_api::OmciAuthState::Error)
}
pub(crate) fn cwmp_epoch(device: &str) -> Option<u64> {
    let r = registry().lock().unwrap();
    if !r.managed {
        return Some(0);
    }
    let serial = r.bindings.get(device)?;
    r.grants
        .values()
        .find(|g| &g.serial == serial && permitted(g))
        .map(|g| g.epoch)
}
pub(crate) fn serial_epoch(serial: &str) -> Option<u64> {
    registry()
        .lock()
        .unwrap()
        .grants
        .values()
        .find(|g| g.serial == serial && permitted(g))
        .map(|g| g.epoch)
}

tokio::task_local! { static EXECUTION_GRANT: (String, u64); }
pub(crate) async fn execution_scope<T>(
    serial: String,
    epoch: u64,
    work: impl std::future::Future<Output = T>,
) -> T {
    EXECUTION_GRANT.scope((serial, epoch), work).await
}
pub(crate) fn execution_grant() -> Option<(String, u64)> {
    EXECUTION_GRANT.try_with(Clone::clone).ok()
}
pub(crate) fn managed() -> bool {
    registry().lock().unwrap().managed
}

pub(crate) fn enqueue_cwmp<T>(
    device: &str,
    epoch: Option<u64>,
    action: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let r = registry().lock().unwrap();
    if !r.managed {
        return action();
    }
    let serial = r.bindings.get(device).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "CWMP identity is not authorized",
        )
    })?;
    enqueue_serial_in(
        &r,
        serial,
        epoch.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "CWMP authorization missing",
            )
        })?,
        action,
    )
}

/// The final nonblocking enqueue and authorization revocation share this lock.
/// No network wait, file persistence or controller lock is allowed in action.
pub(crate) fn enqueue_serial<T>(
    serial: &str,
    epoch: u64,
    action: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    enqueue_serial_in(&registry().lock().unwrap(), serial, epoch, action)
}
fn enqueue_serial_in<T>(
    r: &Registry,
    serial: &str,
    epoch: u64,
    action: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    if r.config.as_ref().is_none_or(|config| {
        !r.grants.values().any(|g| {
            g.serial == serial
                && g.epoch == epoch
                && &g.policy == config.effective(serial)
                && permitted(g)
        })
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "authorization generation changed before publication",
        ));
    }
    action()
}

#[cfg(test)]
thread_local! { static TEST_AUTHORIZED: std::cell::RefCell<std::collections::BTreeSet<String>> = Default::default(); }
#[cfg(test)]
pub(crate) fn test_authorize(serial: &str) {
    TEST_AUTHORIZED.with(|s| {
        s.borrow_mut().insert(serial.into());
    });
}
#[cfg(test)]
pub(crate) fn test_revoke(serial: &str) {
    TEST_AUTHORIZED.with(|s| {
        s.borrow_mut().remove(serial);
    });
}
fn read_credentials(session: &mut OmciSession, layout: Layout) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let loid = session.get(layout.class, 0, layout.loid_mask)?;
    let password = session.get(layout.class, 0, layout.password_mask)?;
    Ok((
        loid.payload[3..3 + layout.loid_width].to_vec(),
        password.payload[3..3 + layout.password_width].to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(serial: &str, policy: Policy) -> Grant {
        Grant {
            serial: serial.into(),
            policy,
            backend: STANDARD,
            token: Arc::new(AtomicBool::new(false)),
            platform: PlatformContext::test_context(),
            state: fttr_api::OmciAuthState::Verified,
            reported: Some(("test-onu".into(), "test-pass".into())),
            epoch: 1,
        }
    }

    #[test]
    fn final_enqueue_rejects_revoked_and_replaced_authorization_generations() {
        let mut r = Registry {
            config: Some(Config {
                global: Policy::Disabled,
                ..Default::default()
            }),
            ..Default::default()
        };
        r.grants.insert(1, grant("TEST00000001", Policy::Disabled));
        let publishes = std::cell::Cell::new(0);
        enqueue_serial_in(&r, "TEST00000001", 1, || {
            publishes.set(publishes.get() + 1);
            Ok(())
        })
        .unwrap();
        r.grants
            .get_mut(&1)
            .unwrap()
            .token
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            enqueue_serial_in::<()>(&r, "TEST00000001", 1, || panic!("revoked publication"))
                .is_err()
        );
        let mut replacement = grant("TEST00000001", Policy::Disabled);
        replacement.epoch = 2;
        r.grants.insert(1, replacement);
        assert!(
            enqueue_serial_in::<()>(&r, "TEST00000001", 1, || panic!("obsolete publication"))
                .is_err()
        );
        enqueue_serial_in(&r, "TEST00000001", 2, || {
            publishes.set(publishes.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(publishes.get(), 2);
    }

    #[test]
    fn reload_revokes_only_changed_effective_policy_and_adapter() {
        let mut original = Config {
            global: Policy::Disabled,
            ..Config::default()
        };
        original
            .devices
            .insert("TEST00000002".into(), Policy::Disabled);
        let mut r = Registry {
            config: Some(original.clone()),
            ..Registry::default()
        };
        r.grants.insert(1, grant("TEST00000001", Policy::Disabled));
        r.grants.insert(2, grant("TEST00000002", Policy::Disabled));
        let mut changed = original.clone();
        changed.global = Policy::LoidPassword {
            loid: b"test-onu".to_vec(),
            password: b"test-pass".to_vec(),
        };
        let mut revoked = Vec::new();
        configure_registry(&mut r, &changed, |id, _| {
            revoked.push(id);
            Ok(())
        })
        .unwrap();
        assert_eq!(revoked, [1]);
        assert!(!permitted(&r.grants[&1]));
        assert!(permitted(&r.grants[&2]));
        // Completing an older exchange cannot undo token cancellation.
        r.grants.get_mut(&1).unwrap().state = fttr_api::OmciAuthState::Verified;
        assert!(!permitted(&r.grants[&1]));
        changed
            .selections
            .insert("TEST00000002".into(), "generic".into());
        revoked.clear();
        configure_registry(&mut r, &changed, |id, _| {
            revoked.push(id);
            Ok(())
        })
        .unwrap();
        assert!(revoked.contains(&2));
        assert!(!permitted(&r.grants[&2]));
    }

    #[test]
    fn revocation_failure_denies_all_changed_grants_without_publishing_policy() {
        let original = Config {
            global: Policy::Disabled,
            ..Config::default()
        };
        let mut r = Registry {
            config: Some(original.clone()),
            ..Registry::default()
        };
        for id in [1, 2] {
            r.grants
                .insert(id, grant(&format!("TEST{id:08}"), Policy::Disabled));
        }
        let changed = Config {
            global: Policy::LoidPassword {
                loid: b"test-onu".to_vec(),
                password: b"test-pass".to_vec(),
            },
            ..Config::default()
        };
        let mut revoked = Vec::new();
        assert!(configure_registry(&mut r, &changed, |id, _| {
            revoked.push(id);
            Err(io::Error::other("test revocation failure"))
        })
        .is_err());
        assert_eq!(revoked, [1, 2]);
        assert!(r.grants.values().all(|g| !permitted(g)));
        assert!(r.config.as_ref() == Some(&original));
    }
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
    use std::collections::VecDeque;

    struct Reply {
        mask: u16,
        set: bool,
        data: Vec<u8>,
        result: u8,
    }
    struct Script {
        replies: Arc<Mutex<VecDeque<Reply>>>,
        pending: Option<BaselineFrame>,
    }
    impl OmciTransport for Script {
        fn send(&mut self, request: &BaselineFrame) -> io::Result<()> {
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected credential operation");
            assert_eq!(request.class_id, STANDARD.class);
            assert_eq!(request.instance_id, 0);
            assert_eq!(
                u16::from_be_bytes([request.payload[0], request.payload[1]]),
                reply.mask
            );
            assert_eq!(request.message_type & 0x1f, if reply.set { 8 } else { 9 });
            let mut payload = [0; 32];
            payload[0] = reply.result;
            if reply.set {
                assert_eq!(request.payload[2], reply.data[0]);
            } else {
                payload[1..3].copy_from_slice(&reply.mask.to_be_bytes());
                payload[3..3 + reply.data.len()].copy_from_slice(&reply.data);
            }
            self.pending = Some(BaselineFrame::new(
                request.transaction_id,
                (request.message_type & 0x1f) | 0x20,
                request.class_id,
                request.instance_id,
                payload,
            ));
            Ok(())
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            Ok(self.pending.take())
        }
    }
    fn exchange(
        status: u8,
        set_result: u8,
        readback: Option<u8>,
    ) -> (OmciSession, Arc<Mutex<VecDeque<Reply>>>) {
        let mut script = VecDeque::from([
            Reply {
                mask: STANDARD.loid_mask,
                set: false,
                data: b"test-onu".to_vec(),
                result: 0,
            },
            Reply {
                mask: STANDARD.password_mask,
                set: false,
                data: b"test-pass".to_vec(),
                result: 0,
            },
            Reply {
                mask: STANDARD.status_mask,
                set: true,
                data: vec![status],
                result: set_result,
            },
        ]);
        if let Some(status) = readback {
            script.push_back(Reply {
                mask: STANDARD.status_mask,
                set: false,
                data: vec![status],
                result: 0,
            });
        }
        let replies = Arc::new(Mutex::new(script));
        (
            OmciSession::new(Script {
                replies: replies.clone(),
                pending: None,
            }),
            replies,
        )
    }
    #[test]
    fn backend_width_denial_still_reports_onu_credentials() {
        let (mut session, remaining) = exchange(1, 0, None);
        remaining.lock().unwrap().pop_back();
        let mut reported = None;
        assert_eq!(
            verify(
                &mut session,
                STANDARD,
                &[b'x'; 25],
                b"test-pass",
                &mut reported,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(reported, Some(("test-onu".into(), "test-pass".into())));
        assert!(remaining.lock().unwrap().is_empty());
    }
    #[test]
    fn success_requires_notification_and_status_readback() {
        let (mut session, remaining) = exchange(1, 0, Some(1));
        let mut reported = None;
        verify(
            &mut session,
            STANDARD,
            b"test-onu",
            b"test-pass",
            &mut reported,
        )
        .unwrap();
        assert_eq!(reported, Some(("test-onu".into(), "test-pass".into())));
        assert!(remaining.lock().unwrap().is_empty());
        let (mut session, _) = exchange(1, 0, Some(0));
        assert_eq!(
            verify(&mut session, STANDARD, b"test-onu", b"test-pass", &mut None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let (mut session, _) = exchange(1, 2, None);
        assert!(verify(&mut session, STANDARD, b"test-onu", b"test-pass", &mut None).is_err());
    }
    #[test]
    fn mismatch_denies_even_if_onu_rejects_notification_and_retains_actual_values() {
        for (loid, password, status) in [
            (b"other-id".as_slice(), b"test-pass".as_slice(), 2),
            (b"test-onu".as_slice(), b"other-pass".as_slice(), 3),
        ] {
            let (mut session, remaining) = exchange(status, 2, None);
            let mut reported = None;
            assert_eq!(
                verify(&mut session, STANDARD, loid, password, &mut reported)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(reported, Some(("test-onu".into(), "test-pass".into())));
            assert!(remaining.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn inheritance_is_whole_policy_and_inactive_fields_are_ignored() {
        assert!(parse_policy("inherit", "", "", false).is_err());
        assert!(parse_policy("password", "", "", true).is_err());
        assert!(parse_policy("loid_password", "id", "", true).is_err());
        assert!(parse_policy("loid_password", "中", "p", true).is_err());
        assert!(parse_policy("loid_password", "id", "1234567890123", true).is_err());
        assert!(matches!(
            parse_policy("disabled", "ignored", "ignored", true),
            Ok(Some(Policy::Disabled))
        ));
        assert!(parse_policy("", "ignored", "ignored", true)
            .unwrap()
            .is_none());
    }
    #[test]
    fn padding_is_not_trimming_or_truncation() {
        assert!(matches_field(b"abc\0\0", b"abc", 5));
        assert!(matches_field(b"abcde", b"abcde", 5));
        assert!(!matches_field(b"abc\0x", b"abc", 5));
        assert!(!matches_field(b"abc  ", b"abc", 5));
        assert!(!matches_field(b"abc", b"abc", 5));
    }
}
