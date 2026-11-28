//! Published management invalidations; health never locks the provisioning controller.
use crate::platform::openwrt::wake::Wake;
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

pub(crate) struct Subscription {
    changed: AtomicBool,
    wake: Arc<Wake>,
}
impl Subscription {
    pub(crate) fn changed(&self) -> bool {
        self.changed.load(Ordering::Acquire)
    }
    fn invalidate(&self) {
        self.changed.store(true, Ordering::Release);
        self.wake.notify();
    }
}

#[derive(Default)]
struct Published {
    enabled: BTreeSet<String>,
    network: String,
    watches: Vec<(String, Weak<Subscription>)>,
}
impl Published {
    fn subscribe(&mut self, serial: &str, wake: Arc<Wake>) -> Arc<Subscription> {
        self.watches.retain(|(_, watch)| watch.strong_count() != 0);
        let watch = Arc::new(Subscription {
            changed: AtomicBool::new(false),
            wake,
        });
        self.watches.push((serial.into(), Arc::downgrade(&watch)));
        watch
    }
    fn config(&mut self, enabled: BTreeSet<String>) {
        self.watches.retain(|(serial, weak)| {
            let Some(watch) = weak.upgrade() else {
                return false;
            };
            if self.enabled.contains(serial) != enabled.contains(serial) {
                watch.invalidate();
            }
            true
        });
        self.enabled = enabled;
    }
    fn network(&mut self, version: String) {
        if self.network == version {
            return;
        }
        self.network = version;
        self.watches.retain(|(serial, weak)| {
            let Some(watch) = weak.upgrade() else {
                return false;
            };
            if self.enabled.contains(serial) {
                watch.invalidate();
            }
            true
        });
    }
}
fn published() -> &'static Mutex<Published> {
    static STATE: OnceLock<Mutex<Published>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
pub(crate) fn subscribe(serial: &str, wake: Arc<Wake>) -> Arc<Subscription> {
    published().lock().unwrap().subscribe(serial, wake)
}
pub(super) fn config(enabled: BTreeSet<String>) {
    published().lock().unwrap().config(enabled);
}
pub(super) fn network(version: String) {
    published().lock().unwrap().network(version);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_affected_serials_invalidate_and_changes_latch_across_aba() {
        let mut state = Published::default();
        state.config(BTreeSet::from(["A".into()]));
        let mut a = state.subscribe("A", Arc::new(Wake::new().unwrap()));
        let mut b = state.subscribe("B", Arc::new(Wake::new().unwrap()));
        state.network("ready-version-epoch1".into());
        assert!(a.changed());
        assert!(!b.changed());
        a = state.subscribe("A", Arc::new(Wake::new().unwrap()));
        state.network("ready-version-epoch1".into());
        state.config(BTreeSet::from(["A".into()]));
        assert!(!a.changed());
        state.network(String::new()); // readiness lost, then restored before a checks
        state.network("ready-version-epoch1".into());
        assert!(a.changed());
        assert!(!b.changed());
        a = state.subscribe("A", Arc::new(Wake::new().unwrap()));
        state.config(BTreeSet::from(["B".into()]));
        assert!(a.changed()); // disabled/removal
        assert!(b.changed()); // enabled
        b = state.subscribe("B", Arc::new(Wake::new().unwrap()));
        state.network("ready-version-epoch2".into());
        assert!(b.changed()); // same resources, new applied authorization epoch
        drop(a);
        drop(b);
        state.network("next".into());
        assert!(state.watches.is_empty());
    }
}
