//! A single arbiter grants dispatch leases; waiting requests hold no response memory.
use super::{
    resources::{self, Permit},
    State, TaskState,
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub(super) struct Dispatcher {
    ready: VecDeque<(String, Instant)>,
    grants: BTreeMap<u64, (Permit, Instant)>,
    effects: BTreeMap<u64, crate::effects::Permit>,
    demand: BTreeMap<String, Instant>,
    pub parsing: std::collections::BTreeSet<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acs::{now, Task};
    use serde_json::json;

    fn queued(state: &mut State, id: u64, device: &str, method: &str) {
        state.tasks.insert(
            id,
            Task {
                id,
                device: device.into(),
                method: method.into(),
                args: json!({}),
                state: TaskState::Queued,
                created: now(),
                sent: None,
                result: None,
            },
        );
    }

    #[test]
    fn saturated_command_lanes_preserve_rpc_dispatch_and_release_global_capacity() {
        let a = crate::effects::Permit::acquire(crate::effects::Kind::Command).unwrap();
        let b = crate::effects::Permit::acquire(crate::effects::Kind::Command).unwrap();
        let mut state = State::default();
        for id in 1..=3 {
            let device = format!("fixture-{id}");
            queued(&mut state, id, &device, "GetParameterValues");
            state.dispatch_demand(&device);
        }
        state.schedule_device("fixture-1");
        assert_eq!(state.dispatch.effects.len(), 2);
        assert!(state.dispatch.grants.contains_key(&1));
        assert!(state.dispatch.grants.contains_key(&2));
        state.tasks.get_mut(&1).unwrap().state = TaskState::Completed;
        state.release_dispatch(1);
        assert!(state.schedule_device("fixture-3"));
        assert!(state.dispatch.grants.contains_key(&3));
        drop((a, b, state));
        crate::effects::Permit::acquire(crate::effects::Kind::Command).unwrap();
    }

    #[test]
    fn mixed_sixteen_device_fifo_does_not_reserve_for_unreachable_heads() {
        let mut state = State::default();
        // The oldest large request has neither an active session nor CR demand.
        queued(&mut state, 1, "offline", "GetParameterNames");
        for device in 1..16 {
            let name = format!("cpe-{device}");
            for batch in 0..2 {
                queued(
                    &mut state,
                    (batch * 16 + device + 1) as u64,
                    &name,
                    if device % 3 == 0 {
                        "GetParameterNames"
                    } else if device % 3 == 1 {
                        "GetParameterValues"
                    } else {
                        "SetParameterValues"
                    },
                );
            }
            state.dispatch_demand(&name);
        }
        let mut completed = 0;
        for _ in 0..32 {
            state.schedule_device("cpe-1");
            assert!(state.budget.used() <= resources::RESULT_BYTES);
            let grants: Vec<_> = state.dispatch.grants.keys().copied().collect();
            if grants.is_empty() {
                break;
            }
            for id in grants {
                let task = &state.tasks[&id];
                assert_ne!(task.device, "offline");
                assert!(
                    !state.tasks.values().any(|other| other.device == task.device
                        && other.id < id
                        && other.state == TaskState::Queued),
                    "per-device FIFO bypass"
                );
                state.tasks.get_mut(&id).unwrap().state = TaskState::Completed;
                state.release_dispatch(id);
                completed += 1;
            }
        }
        assert_eq!(completed, 30);
        assert_eq!(state.tasks[&1].state, TaskState::Queued);
        assert_eq!(state.budget.used(), 0);
    }

    #[test]
    fn aged_large_waiter_drains_younger_work_but_preserves_small_lane() {
        let mut state = State::default();
        let held = state
            .budget
            .reserve(2 * 1024 * 1024, resources::RESULT_BYTES)
            .unwrap();
        queued(&mut state, 1, "large", "GetParameterNames");
        queued(&mut state, 2, "medium", "GetParameterValues");
        queued(&mut state, 3, "small", "SetParameterValues");
        for name in ["large", "medium", "small"] {
            state.dispatch_demand(name);
        }
        state
            .dispatch
            .ready
            .push_back(("large".into(), Instant::now() - Duration::from_secs(11)));
        assert!(state.schedule_device("small"));
        assert!(!state.dispatch.grants.contains_key(&1));
        assert!(!state.dispatch.grants.contains_key(&2));
        drop(held);
        assert!(state.schedule_device("large"));
        assert!(state.dispatch.grants.contains_key(&3));
    }

    #[test]
    fn cancellation_retains_the_inflight_parser_charge_until_drop() {
        let mut state = State::default();
        queued(&mut state, 1, "cpe", "GetParameterNames");
        assert!(state.schedule_device("cpe"));
        let parsing = state.parsing_permit(1).unwrap();
        state.tasks.get_mut(&1).unwrap().state = TaskState::Unknown;
        state.release_dispatch(1);
        assert_eq!(state.budget.used(), 2 * 1024 * 1024);
        drop(parsing);
        assert_eq!(state.budget.used(), 0);
    }
}
impl State {
    pub(super) fn resources_view(&self) -> serde_json::Value {
        let reserved: usize = self.dispatch.grants.values().map(|(p, _)| p.bytes()).sum();
        let retained: usize = self
            .tasks
            .values()
            .filter_map(|t| t.result.as_ref())
            .map(|r| r.bytes())
            .sum();
        serde_json::json!({
            "protocol_effect_limit": crate::effects::SLOTS,
            "protocol_effect_working_bytes": crate::effects::SLOTS * crate::effects::SLOT_BYTES,
            "rpc_effects":self.dispatch.effects.len(),
            "rpc_effect_reserved_bytes":self.dispatch.effects.values().map(crate::effects::Permit::bytes).sum::<usize>(),
            "used_bytes": self.budget.used(), "limit_bytes": resources::RESULT_BYTES,
            "dispatch_bytes": reserved, "retained_bytes": retained,
            "consumer_or_parser_bytes": self.budget.used().saturating_sub(reserved + retained),
            "argument_bytes": self.tasks.values().filter(|t| t.state.is_active()).map(|t| resources::memory(&t.args)).sum::<usize>(),
            "parsing": self.dispatch.parsing.len(),
            "owners": self.tasks.values().filter(|t| t.state.is_active() || t.result.is_some()).map(|t| serde_json::json!({
                "id":t.id,"device":t.device,"method":t.method,"state":t.state,
                "reserved_bytes":self.dispatch.grants.get(&t.id).map_or(0, |(p,_)| p.bytes()),
                "result_bytes":t.result.as_ref().map_or(0, |r| r.bytes()),
                "waiting_seconds":if t.state == TaskState::Queued { super::now().saturating_sub(t.created) } else { 0 }
            })).collect::<Vec<_>>()
        })
    }
    // Only an active session or an eligible connection-request attempt can
    // consume a lease. An unreachable queue head must not hold up live CPEs.
    pub(super) fn dispatch_demand(&mut self, device: &str) {
        self.dispatch.demand.insert(device.into(), Instant::now());
    }
    pub(super) fn schedule_device(&mut self, device: &str) -> bool {
        self.dispatch_demand(device);
        let at = Instant::now();
        self.dispatch
            .demand
            .retain(|_, at| at.elapsed() < Duration::from_secs(10));
        self.dispatch.grants.retain(|id, (_, since)| {
            self.tasks.get(id).is_some_and(|t| {
                t.state == TaskState::Sent
                    || t.state == TaskState::Queued && since.elapsed() < Duration::from_secs(10)
            })
        });
        self.dispatch
            .effects
            .retain(|id, _| self.dispatch.grants.contains_key(id));
        // Register all accepted heads in durable ID order, rather than letting
        // HTTP arrival order or configuration iteration choose the winner.
        for task in self.tasks.values().filter(|t| {
            t.state == TaskState::Queued && self.dispatch.demand.contains_key(&t.device)
        }) {
            if !self.dispatch.grants.contains_key(&task.id)
                && !self.dispatch.ready.iter().any(|(d, _)| d == &task.device)
            {
                self.dispatch.ready.push_back((task.device.clone(), at));
            }
        }
        let mut rounds = self.dispatch.ready.len();
        let mut draining = None;
        while rounds > 0 {
            rounds -= 1;
            let Some((candidate, since)) = self.dispatch.ready.pop_front() else {
                break;
            };
            if !self.dispatch.demand.contains_key(&candidate) {
                continue;
            }
            if self
                .tasks
                .values()
                .any(|t| t.device == candidate && t.state == TaskState::Sent)
            {
                continue;
            }
            let Some(task) = self
                .tasks
                .values()
                .find(|t| t.device == candidate && t.state == TaskState::Queued)
            else {
                continue;
            };
            if self.dispatch.grants.contains_key(&task.id) {
                continue;
            }
            let bytes = resources::reservation(&task.method);
            if draining.is_some() && bytes > 64 * 1024 {
                self.dispatch.ready.push_back((candidate, since));
                continue;
            }
            let ceiling = if bytes > 64 * 1024 {
                resources::RESULT_BYTES - resources::SMALL_LANE
            } else {
                resources::RESULT_BYTES
            };
            if let Some(permit) = self.budget.reserve(bytes, ceiling) {
                let Ok(effect) = crate::effects::Permit::acquire(crate::effects::Kind::Rpc) else {
                    self.dispatch.ready.push_back((candidate, since));
                    continue;
                };
                self.dispatch.effects.insert(task.id, effect);
                self.dispatch.grants.insert(task.id, (permit, at));
            } else {
                // Once aged, let in-flight work drain rather than continually
                // filling its space with younger requests (large-request starvation).
                let aged = since.elapsed() >= Duration::from_secs(10);
                if aged && bytes > 64 * 1024 {
                    draining = Some((candidate, since));
                } else {
                    self.dispatch.ready.push_back((candidate, since));
                }
            }
        }
        if let Some(waiter) = draining {
            self.dispatch.ready.push_front(waiter);
        }
        self.tasks
            .values()
            .find(|t| t.device == device && t.state == TaskState::Queued)
            .is_some_and(|t| self.dispatch.grants.contains_key(&t.id))
    }
    pub(super) fn take_permit(&mut self, id: u64) -> Option<Permit> {
        self.dispatch.effects.remove(&id);
        self.dispatch.grants.remove(&id).map(|(p, _)| p)
    }
    pub(super) fn parsing_permit(&self, id: u64) -> Option<Permit> {
        self.dispatch.grants.get(&id).map(|(p, _)| p.clone())
    }
    pub(super) fn release_dispatch(&mut self, id: u64) {
        self.dispatch.effects.remove(&id);
        self.dispatch.grants.remove(&id);
    }
    pub(super) fn release_terminal_dispatches(&mut self) {
        let active = |id: &u64| {
            self.tasks
                .get(id)
                .is_some_and(|task| task.state.is_active())
        };
        self.dispatch.grants.retain(|id, _| active(id));
        self.dispatch.effects.retain(|id, _| active(id));
    }
    pub(super) fn waiting_capacity(&self, id: u64) -> bool {
        self.tasks
            .get(&id)
            .is_some_and(|t| t.state == TaskState::Queued)
            && !self
                .tasks
                .values()
                .any(|t| t.device == self.tasks[&id].device && t.state == TaskState::Sent)
            && !self.dispatch.grants.contains_key(&id)
    }
}
