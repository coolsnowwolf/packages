//! Scheduling facts come from protocol results, never the rendered status view.
use super::*;

pub(super) struct Facts {
    pub phase: Option<OperationPhase>,
    pub apply: bool,
    pub revision: String,
    pub completed_at: Option<u64>,
    pub unresolved: bool,
    pub reconcile: bool,
    pub drift: bool,
    pub verified: bool,
    pub failed: bool,
}

impl Facts {
    fn collect<'a>(
        phase: Option<OperationPhase>,
        apply: bool,
        revision: String,
        completed_at: Option<u64>,
        unresolved: bool,
        reconcile: bool,
        items: impl Iterator<Item = &'a Value>,
        lifecycle: impl Iterator<Item = &'a Value>,
    ) -> Self {
        let mut facts = Self {
            phase,
            apply,
            revision,
            completed_at,
            unresolved: unresolved || (phase == Some(OperationPhase::Interrupted) && apply),
            reconcile,
            drift: false,
            failed: false,
            verified: phase == Some(OperationPhase::Completed),
        };
        let mut count = 0;
        for item in items {
            count += 1;
            facts.observe(item, false);
        }
        facts.verified &= count > 0;
        for item in lifecycle {
            facts.observe(item, true);
        }
        facts.verified &= !facts.unresolved && !facts.reconcile;
        facts
    }

    fn observe(&mut self, value: &Value, lifecycle: bool) {
        let state = ItemOutcome::from_result(value);
        self.unresolved |= matches!(
            state,
            Some(ItemOutcome::Unknown | ItemOutcome::AcceptedPendingApply)
        );
        self.reconcile |= state == Some(ItemOutcome::ReconcileRequired);
        self.drift |= state == Some(ItemOutcome::Drift);
        self.failed |= matches!(state, Some(ItemOutcome::Blocked | ItemOutcome::Failed));
        self.verified &= matches!(state, Some(ItemOutcome::Verified))
            || (lifecycle
                && matches!(
                    state,
                    Some(ItemOutcome::Created | ItemOutcome::Deleted | ItemOutcome::Disabled)
                ));
    }

    // History decoding is a boundary operation. It does not render an Operation
    // or infer a successful write from phase=completed.
    pub fn recovered(snapshot: &Value) -> Self {
        let phase = serde::Deserialize::deserialize(&snapshot["state"]).ok();
        let mut facts = Self::collect(
            phase,
            snapshot["mode"] != "plan",
            snapshot["revision"].as_str().unwrap_or("").into(),
            snapshot["completed_at"].as_u64(),
            snapshot["unresolved_operation"].is_object() || snapshot["effect_uncertain"] == true,
            snapshot["reconcile_required"] == true,
            snapshot["items"].as_array().into_iter().flatten(),
            ["wan_lifecycle", "wifi_lifecycle"]
                .into_iter()
                .flat_map(|key| {
                    snapshot[key]
                        .as_object()
                        .into_iter()
                        .flat_map(|m| m.values())
                }),
        );
        if snapshot["backend"]["effective"] == "command" {
            facts.verified &= snapshot["readback_verified"] == true;
        }
        facts
    }
}

impl Operation {
    pub(super) fn facts(&self) -> Facts {
        if let Some(command) = self.command() {
            let result = command.result.as_ref().unwrap_or(&Value::Null);
            let mut facts = Facts::collect(
                Some(self.stage),
                self.apply,
                self.revision.clone(),
                self.completed_at,
                result["unresolved_operation"].is_object() || result["effect_uncertain"] == true,
                false,
                result["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(self.results.values()),
                ["wan_lifecycle", "wifi_lifecycle"]
                    .into_iter()
                    .flat_map(|key| result[key].as_object().into_iter().flat_map(|m| m.values())),
            );
            facts.verified &= result["readback_verified"] == true;
            facts
        } else if let super::Execution::Acs(state) = &self.execution {
            let mut facts = Facts::collect(
                Some(self.stage),
                self.apply,
                self.revision.clone(),
                self.completed_at,
                false,
                false,
                self.results.values(),
                state
                    .wan
                    .results
                    .values()
                    .chain(state.wifi.results.values()),
            );
            facts.verified &= self.device.items.iter().all(|i| {
                self.results
                    .get(&i.id)
                    .is_some_and(|r| ItemOutcome::from_result(r) == Some(ItemOutcome::Verified))
            });
            facts
        } else {
            Facts::collect(
                Some(self.stage),
                self.apply,
                self.revision.clone(),
                self.completed_at,
                false,
                false,
                self.results.values(),
                std::iter::empty(),
            )
        }
    }
}

impl Controller {
    pub(super) fn unresolved_owner(&self, device: &Device) -> bool {
        self.operations.values().any(|op| {
            (op.device.id == device.id || op.device.optical_sn == device.optical_sn)
                && op.facts().unresolved
        }) || self.recovered.values().any(|op| {
            (op["device"] == device.id || op["optical_sn"] == device.optical_sn)
                && Facts::recovered(op).unresolved
        })
    }

    pub(super) fn previous_facts(&self, device: &str) -> Option<Facts> {
        self.business_operation(device)
            .map(Operation::facts)
            .or_else(|| self.recovered.get(device).map(Facts::recovered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_items_never_hide_unsettled_lifecycle_or_native_execution() {
        for outcome in [
            "unknown",
            "accepted_pending_apply",
            "blocked",
            "failed",
            "drift",
            "reconcile_required",
        ] {
            let mut op = super::super::tests::operation(OperationPhase::Completed, false);
            op.results
                .insert("wifi".into(), json!({"state":"verified"}));
            op.wan
                .results
                .insert("final_inventory".into(), json!({"state":outcome}));
            assert!(!op.facts().verified, "{outcome}");
            assert!(!Facts::recovered(&op.view()).verified, "{outcome}");
        }
        let mut op = super::super::tests::operation(OperationPhase::Completed, false);
        op.device.provisioning_adapter = "hl202-mqtt".into();
        for complete in [false, true] {
            for unresolved in [false, true] {
                op.execution = Execution::Command(CommandExecution {
                    result: Some(
                        json!({"items":[{"state":"verified"}],"readback_verified":complete,"unresolved_operation":if unresolved {json!({"operation_id":9})} else {Value::Null}}),
                    ),
                    ..Default::default()
                });
                assert_eq!(op.facts().verified, complete && !unresolved);
                assert_eq!(
                    Facts::recovered(&op.view()).verified,
                    complete && !unresolved
                );
            }
        }
    }
}
