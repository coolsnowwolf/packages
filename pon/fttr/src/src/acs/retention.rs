//! Bounded consumer leases. Only the in-process transport can acquire/ack leases.
use super::{now, State};
use serde::{Deserialize, Serialize};

pub(super) const LEASE_SECONDS: u64 = 120;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct ResultInfo {
    pub completed_at: Option<u64>,
    pub protected: bool,
    #[serde(rename = "result_status")]
    pub status: String,
    #[serde(rename = "cleanup_reason")]
    pub reason: String,
    /// Compact safety evidence outlives the ordinary raw-response cache.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub management_services: std::collections::BTreeMap<String, bool>,
}
impl ResultInfo {
    pub fn protected() -> Self {
        Self {
            protected: true,
            status: "pending".into(),
            ..Self::default()
        }
    }
}
impl State {
    pub(super) fn ack_result(&mut self, id: u64) -> bool {
        let Some(info) = self.results.get_mut(&id) else {
            return false;
        };
        if !info.protected || info.completed_at.is_none() {
            return false;
        }
        info.protected = false;
        info.status = "released".into();
        info.reason = "consumer owns shared response".into();
        // The returned handle keeps the charge; history stores compact evidence.
        if let Some(task) = self.tasks.get_mut(&id) {
            task.result = None;
        }
        true
    }
    pub(super) fn mark_completed(&mut self, id: u64) {
        let info = self.results.entry(id).or_default();
        info.completed_at.get_or_insert_with(now);
        info.status = "available".into();
        if let Some(task) = self.tasks.get_mut(&id) {
            task.release_request();
            crate::provisioning::controller::wake_device(&task.device);
        }
    }
}
