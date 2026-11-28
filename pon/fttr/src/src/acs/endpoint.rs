//! Endpoint replacement and retained-listener migration policy.
use super::*;

/// Change only the endpoint. Device/task history is shared with draining sessions.
pub fn reconfigure(
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
    retain_previous: bool,
) -> Result<(), String> {
    let _guard = RECONFIGURE.lock().unwrap();
    let next = transition(
        init(),
        enabled,
        listen,
        path,
        username,
        password,
        retain_previous,
    )?;
    *SERVER.get().unwrap().lock().unwrap() = next;
    Ok(())
}

pub fn configuration_matches(
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
) -> bool {
    let s = init();
    s.enabled == enabled
        && s.listen == listen
        && s.path == path
        && s.auth == (!username.is_empty()).then(|| http::basic(username, password))
        && s.failure.lock().unwrap().is_none()
        && (!enabled || (!s.stopping.load(Ordering::Acquire) && !s.stopped.load(Ordering::Acquire)))
}

pub fn retire_previous() {
    let _guard = RECONFIGURE.lock().unwrap();
    let s = init();
    for previous in std::mem::take(&mut *s.legacy.lock().unwrap()) {
        stop_listener(&previous);
    }
}

pub(super) fn stop_listener(server: &Server) {
    *server.drain_deadline.lock().unwrap() =
        Some(std::time::Instant::now() + Duration::from_secs(30));
    server.stopping.store(true, Ordering::Release);
}

pub(super) fn transition(
    old: Arc<Server>,
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
    retain_previous: bool,
) -> Result<Arc<Server>, String> {
    if crate::runtime::is_stopping() {
        return Err("ACS is stopping".into());
    }
    if let Some(reason) = &old.state.lock().unwrap().load_failure {
        return Err(reason.clone());
    }
    let auth = (!username.is_empty()).then(|| http::basic(username, password));
    if old.enabled == enabled
        && old.listen == listen
        && old.path == path
        && old.auth == auth
        && old.failure.lock().unwrap().is_none()
        && (!enabled || !old.stopped.load(Ordering::Acquire))
    {
        return Ok(old);
    }
    let listener = if enabled {
        if old.enabled && old.listen == listen {
            stop_listener(&old);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !old.stopped.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err("old ACS listener did not stop".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Some(TcpListener::bind(listen).map_err(|e| format!("ACS listener bind failed: {e}"))?)
    } else {
        None
    };
    if let Some(listener) = &listener {
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("ACS listener setup failed: {e}"))?;
    }
    let endpoint_id = token().map_err(|_| "cannot allocate ACS endpoint identity")?;
    let actual_listen = listener
        .as_ref()
        .map(|l| l.local_addr().map(|a| a.to_string()))
        .transpose()
        .map_err(|_| "cannot inspect ACS listener")?
        .unwrap_or_else(|| listen.into());
    let mut legacy = old.legacy.lock().unwrap().clone();
    if retain_previous && enabled && old.enabled {
        legacy.push(old.clone());
    } else {
        for previous in &legacy {
            stop_listener(previous);
        }
        legacy.clear();
    }
    let server = Arc::new(Server {
        state: old.state.clone(),
        enabled,
        listen: actual_listen,
        path: path.into(),
        path_policy: old.path_policy.clone(),
        state_file: old.state_file.clone(),
        auth,
        failure: Mutex::new(None),
        requests: old.requests.clone(),
        endpoint_id,
        drain_deadline: Mutex::new(None),
        legacy: Mutex::new(legacy),
        stopping: AtomicBool::new(false),
        stopped: AtomicBool::new(!enabled),
    });
    if !retain_previous || !enabled || old.listen == listen {
        stop_listener(&old);
    }
    if let Some(listener) = listener {
        listener::start(server.clone(), listener);
    }
    Ok(server)
}

static EXTRA_SERVER: OnceLock<Mutex<Option<Arc<Server>>>> = OnceLock::new();
static LAN_PATH_POLICY: OnceLock<Arc<Mutex<PathPolicy>>> = OnceLock::new();

fn extra_slot() -> &'static Mutex<Option<Arc<Server>>> {
    EXTRA_SERVER.get_or_init(|| Mutex::new(None))
}

pub(super) fn lan_path_policy() -> &'static Arc<Mutex<PathPolicy>> {
    LAN_PATH_POLICY.get_or_init(|| Arc::new(Mutex::new(PathPolicy::default())))
}

pub(super) fn extra_endpoint_id() -> Option<String> {
    let slot = extra_slot().lock().unwrap();
    slot.as_ref().and_then(|s| {
        (s.enabled && !s.stopping.load(Ordering::Acquire) && !s.stopped.load(Ordering::Acquire))
            .then(|| s.endpoint_id.clone())
    })
}

pub(super) fn extra_listener_active() -> bool {
    extra_endpoint_id().is_some()
}

fn same_port_wildcard(primary_listen: &str, target_listen: &str) -> bool {
    let Some(target_port) = target_listen.rsplit(':').next() else {
        return false;
    };
    primary_listen == format!("0.0.0.0:{target_port}")
}

/// Start, update, or stop an auxiliary LAN ACS listener sharing state and path policy with the primary endpoint.
pub fn configure_extra_listener(addr: Option<&str>) -> Result<(), String> {
    let _guard = RECONFIGURE.lock().unwrap();
    let primary = init();
    let Some(target) = addr.map(str::trim).filter(|s| !s.is_empty()) else {
        if let Some(existing) = extra_slot().lock().unwrap().take() {
            stop_listener(&existing);
        }
        return Ok(());
    };
    if crate::runtime::is_stopping() {
        return Err("ACS extra listener cannot start while runtime is stopping".into());
    }
    if let Some(reason) = &primary.state.lock().unwrap().load_failure {
        return Err(reason.clone());
    }
    if primary.enabled
        && !primary.stopped.load(Ordering::Acquire)
        && (primary.listen == target || same_port_wildcard(&primary.listen, target))
    {
        if let Some(existing) = extra_slot().lock().unwrap().take() {
            stop_listener(&existing);
        }
        return Ok(());
    }
    let existing = extra_slot().lock().unwrap().clone();
    if let Some(old) = existing.as_ref() {
        if old.listen == target
            && old.path == primary.path
            && old.auth == primary.auth
            && old.failure.lock().unwrap().is_none()
            && !old.stopping.load(Ordering::Acquire)
            && !old.stopped.load(Ordering::Acquire)
        {
            return Ok(());
        }
        stop_listener(old);
        if old.listen == target {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !old.stopped.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err("old extra ACS listener did not stop".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let listener =
        TcpListener::bind(target).map_err(|e| format!("ACS extra listener bind failed: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("ACS extra listener setup failed: {e}"))?;
    let endpoint_id = token().map_err(|_| "cannot allocate extra ACS endpoint identity")?;
    let actual_listen = listener
        .local_addr()
        .map(|a| a.to_string())
        .map_err(|_| "cannot inspect extra ACS listener")?;
    let server = Arc::new(Server {
        state: primary.state.clone(),
        enabled: true,
        listen: actual_listen,
        path: primary.path.clone(),
        path_policy: lan_path_policy().clone(),
        state_file: primary.state_file.clone(),
        auth: primary.auth.clone(),
        failure: Mutex::new(None),
        requests: primary.requests.clone(),
        endpoint_id,
        drain_deadline: Mutex::new(None),
        legacy: Mutex::new(Vec::new()),
        stopping: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    });
    *extra_slot().lock().unwrap() = Some(server.clone());
    listener::start(server, listener);
    Ok(())
}

pub fn extra_listener_addr() -> Option<String> {
    let slot = extra_slot().lock().unwrap();
    slot.as_ref().and_then(|s| {
        (!s.stopping.load(Ordering::Acquire) && !s.stopped.load(Ordering::Acquire))
            .then(|| s.listen.clone())
    })
}
