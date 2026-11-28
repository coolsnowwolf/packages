//! Bounded handler timings; never retain arguments or results.
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};

fn state() -> &'static Mutex<BTreeMap<String, fttr_api::UbusMethodStats>> {
    static STATE: OnceLock<Mutex<BTreeMap<String, fttr_api::UbusMethodStats>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn record(method: &str, elapsed: Duration, failed: bool) {
    let Ok(mut state) = state().try_lock() else {
        return;
    };
    let stats = state.entry(method.into()).or_default();
    stats.calls = stats.calls.saturating_add(1);
    stats.failures = stats.failures.saturating_add(u64::from(failed));
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    stats.last_us = micros;
    stats.max_us = stats.max_us.max(micros);
    stats.total_us = stats.total_us.saturating_add(micros);
}

pub(super) fn snapshot() -> Option<BTreeMap<String, fttr_api::UbusMethodStats>> {
    Some(state().try_lock().ok()?.clone())
}
