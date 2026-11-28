//! Bounded command-effect admission. Waiting operations hold no transport future.
use super::*;
use crate::provisioning::command::Job;

const MAX_COMMAND_EFFECTS: usize = 2;
const COMMAND_WORKING_BYTES: usize = 20 * 1024 * 1024;
const INVENTORY_ALLOWANCE: usize = 8 * 1024 * 1024;

// Reservation, not a measured RSS claim: allow for framed output, parsed field
// trees, desired/diff/script and readback copies. Count desired input without
// constructing a JSON buffer (and never log its credential values).
fn reservation(device: &Device) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(std::mem::size_of::<Device>());
    for item in &device.items {
        count.0 = count
            .0
            .saturating_add(std::mem::size_of_val(item))
            .saturating_add(item.id.len());
        // These models contain only JSON-serializable, string-keyed values.
        let _ = serde_json::to_writer(&mut count, &item.parameters);
        let endpoint_bytes = |endpoint: &super::super::model::EndpointRef| {
            std::mem::size_of_val(endpoint)
                + match endpoint {
                    super::super::model::EndpointRef::Named(name) => name.capacity(),
                    _ => 0,
                }
        };
        if let Some(wlan) = &item.wlan_intent {
            count.0 = count.0.saturating_add(
                std::mem::size_of_val(wlan)
                    + wlan.ssid.capacity()
                    + wlan.key.as_ref().map_or(0, String::capacity)
                    + endpoint_bytes(&wlan.endpoint),
            );
        }
        if let Some(wan) = &item.wan_intent {
            count.0 = count.0.saturating_add(std::mem::size_of_val(wan));
            for service in &wan.services {
                count.0 = count
                    .0
                    .saturating_add(std::mem::size_of_val(service) + service.capacity());
            }
            for endpoint in &wan.bindings {
                count.0 = count.0.saturating_add(endpoint_bytes(endpoint));
            }
        }
        if let Some(binding) = &item.binding_intent {
            for endpoint in &binding.endpoints {
                count.0 = count.0.saturating_add(endpoint_bytes(endpoint));
            }
        }
    }
    INVENTORY_ALLOWANCE.saturating_add(count.0.saturating_mul(4))
}

impl Operation {
    pub(super) fn finish_command(
        &mut self,
        outcome: Result<Value, crate::provisioning::command::Failure>,
        time: u64,
    ) {
        if self.recovers().is_some() {
            let result = match outcome {
                Ok(recovery) => recovery,
                Err(failure) => json!({
                    "settled": false,
                    "recovery_outcome": "retryable_failure",
                    "recovery_state": "retry_scheduled",
                    "observation_error": failure.reason,
                    "write_performed": false,
                    "readback_verified": false,
                    "write_attempted": false,
                    "terminal_received": false,
                    "write_verified": false,
                    "items": self.device.items.iter().map(|i| json!({
                        "id": i.id,
                        "kind": i.kind,
                        "service_type": i.service_type,
                        "state": "unknown",
                        "reason": failure.reason,
                    })).collect::<Vec<_>>(),
                }),
            };
            let state = self.command_mut().unwrap();
            state.result = Some(result);
            state.job = None;
            state.reservation = 0;
            self.complete(time);
            return;
        }
        if self.observes().is_some() {
            let key = if self.command().unwrap().reboot_from.is_some() {
                "restart"
            } else {
                "observation"
            };
            let result = match outcome {
                Ok(observation) => json!({(key):observation}),
                Err(failure) => json!({"observation_error":failure.reason}),
            };
            let state = self.command_mut().unwrap();
            state.result = Some(result);
            state.job = None;
            state.reservation = 0;
            self.complete(time);
            return;
        }
        let result = outcome.unwrap_or_else(|failure| json!({"items":self.device.items.iter().map(|i| json!({"id":i.id,"kind":i.kind,"service_type":i.service_type,"state":if failure.uncertain {ItemOutcome::Unknown} else {ItemOutcome::Blocked},"reason":failure.reason})).collect::<Vec<_>>(),"readback_verified":false,"effect_uncertain":failure.uncertain}));
        let state = self
            .command_mut()
            .expect("command completion belongs to command owner");
        state.result = Some(result);
        state.job = None;
        state.reservation = 0;
        self.complete(time);
    }
}

impl Controller {
    // Harvest independently of current authorization/reachability. An old effect
    // still belongs to this operation after revocation; it must not be forgotten.
    pub(super) fn collect_commands(&mut self, time: u64) {
        for op in self.operations.values_mut() {
            if let Some(outcome) = op
                .command_mut()
                .and_then(|state| state.job.as_mut())
                .and_then(Job::take)
            {
                op.finish_command(outcome, time);
                self.dirty = true;
            }
        }
    }

    pub(super) fn admit_commands(
        &mut self,
        reachable: &BTreeSet<String>,
        time: u64,
        mut launch: impl FnMut(&Operation) -> Job,
    ) {
        if crate::runtime::is_stopping() {
            return;
        }
        let mut active = self
            .operations
            .values()
            .filter(|op| op.command_running())
            .count();
        let mut bytes: usize = self
            .operations
            .values()
            .map(|op| op.command().map_or(0, |state| state.reservation))
            .sum();
        let ids: Vec<_> = self.operations.keys().cloned().collect();
        let first = self
            .command_cursor
            .as_ref()
            .map_or(0, |cursor| ids.partition_point(|id| id <= cursor));
        for id in ids[first..].iter().chain(&ids[..first]) {
            if active >= MAX_COMMAND_EFFECTS {
                break;
            }
            let op = self.operations.get_mut(id).unwrap();
            if op
                .command()
                .is_some_and(|state| state.reboot_from.is_some() && state.result.is_none())
                && !op.command_running()
                && time.checked_sub(op.started).is_none_or(|age| age > 30)
            {
                op.finish_command(Err("recovery restart expired before dispatch".into()), time);
                self.dirty = true;
                continue;
            }
            if op.device.backend_kind() != Backend::Command
                || op.command_running()
                || matches!(
                    op.stage,
                    OperationPhase::Completed | OperationPhase::Interrupted
                )
                || !reachable.contains(id)
                || self.resets.blocks(&op.device.optical_sn)
                || !crate::omci_auth::allowed(&op.device.optical_sn)
            {
                continue;
            }
            let required = reservation(&op.device);
            if required > COMMAND_WORKING_BYTES {
                op.finish_command(
                    Err("command desired state exceeds working-set admission budget".into()),
                    time,
                );
                self.dirty = true;
                continue;
            }
            if bytes.saturating_add(required) > COMMAND_WORKING_BYTES {
                continue;
            }
            let job = launch(op);
            let state = op
                .command_mut()
                .expect("command backend owns command state");
            state.job = Some(job);
            state.reservation = required;
            op.work_started = true;
            op.stage = OperationPhase::Reading;
            self.command_cursor = Some(id.clone());
            self.dirty = true;
            active += 1;
            bytes += required;
        }
    }

    pub(super) fn compact_completed(&mut self) {
        let ids: Vec<_> = self
            .operations
            .iter()
            .filter(|(_, op)| op.stage == OperationPhase::Completed && !op.command_running())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let op = self.operations.remove(&id).unwrap();
            if op.recovers().is_some() {
                let view = op.view();
                self.observations.insert(id.clone(), view.clone());
                if view["settled"] == true {
                    let mut settled = view;
                    let matches_current = settled["settled_matches_current"] == true;
                    settled["mode"] =
                        json!(if settled["write_performed"] == true || matches_current {
                            "apply"
                        } else {
                            "plan"
                        });
                    if !matches_current {
                        settled["revision"] = settled["settled_revision"].clone();
                    }
                    let rev = settled["revision"].clone();
                    let settled_op_id = settled["operation_id"].clone();
                    if let Some(items) = settled["items"].as_array_mut() {
                        for item in items {
                            item["revision"] = rev.clone();
                            item.as_object_mut().map(|m| {
                                m.entry("operation_id")
                                    .or_insert_with(|| settled_op_id.clone())
                            });
                        }
                    }
                    settled["unresolved_operation"] = Value::Null;
                    settled["effect_uncertain"] = json!(false);
                    settled["wifi_stage"] = json!("done");
                    self.read_recovery.remove(&id);
                    self.attempts.remove(&id);
                    self.next_retry_at.remove(&id);
                    self.recovered.insert(id.clone(), settled);
                    if !matches_current {
                        wake_device(&id);
                    }
                }
            } else if op.observes().is_some() {
                self.observations.insert(id, op.view());
            } else {
                self.recovered.insert(id, op.view());
            }
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_compaction_preserves_unresolved_facts_and_history_without_hot_device_state() {
        let mut c = Controller::default();
        let mut op = super::super::tests::operation(OperationPhase::Completed, false);
        op.results.insert(
            "wifi".into(),
            json!({"state":"unknown","reason":"uncertain old write"}),
        );
        c.operations.insert("child".into(), op);
        let history = c.history();
        let id = c.operations["child"].id;
        let by_id = c.operation_view(id).unwrap();
        c.compact_completed();
        assert!(c.operations.is_empty());
        assert!(c.previous_facts("child").unwrap().unresolved);
        assert_eq!(c.history(), history);
        assert_eq!(c.operation_view(id), Some(by_id));
        assert!(c.operation_view(id + 1).is_none());
    }

    #[tokio::test]
    async fn admission_is_bounded_fair_and_harvests_after_authorization_revocation() {
        let mut c = Controller::default();
        let mut reachable = BTreeSet::new();
        for n in 0..5 {
            let mut op = super::super::tests::operation(OperationPhase::Waiting, false);
            op.device.id = format!("child{n}");
            op.device.optical_sn = format!("H3CT000000{n:02}");
            op.device.provisioning_adapter = "hl202-mqtt".into();
            op.execution = Execution::Command(CommandExecution::default());
            crate::omci_auth::test_authorize(&op.device.optical_sn);
            reachable.insert(op.device.id.clone());
            c.operations.insert(op.device.id.clone(), op);
        }
        let mut sends = Vec::new();
        let mut launch = |op: &Operation| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            sends.push((op.device.id.clone(), tx));
            Job::test_spawn(async move { rx.await.unwrap() })
        };
        c.admit_commands(&reachable, 100, &mut launch);
        assert_eq!(
            c.operations
                .values()
                .filter(|o| o.command_running())
                .count(),
            2
        );
        c.admit_commands(&reachable, 100, &mut launch);
        drop(launch);
        assert_eq!(
            sends.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["child0", "child1"]
        );
        // Revoke one device and disconnect both; terminal effects must still drain.
        crate::omci_auth::test_revoke(&c.operations["child0"].device.optical_sn);
        for (_, tx) in sends {
            tx.send(Err(crate::provisioning::command::Failure::uncertain(
                "injected completion",
            )))
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                c.collect_commands(101);
                if c.operations.values().all(|o| !o.command_running()) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(c.previous_facts("child0").unwrap().unresolved);
        let mut next = Vec::new();
        c.admit_commands(&reachable, 102, |op| {
            next.push(op.device.id.clone());
            Job::test_spawn(std::future::pending())
        });
        assert_eq!(next, ["child2", "child3"]);
    }

    #[test]
    fn oversized_desired_input_is_rejected_before_allocating_effect() {
        let mut c = Controller::default();
        let mut op = super::super::tests::operation(OperationPhase::Waiting, false);
        op.device.provisioning_adapter = "hl202-mqtt".into();
        op.device.items[0].parameters[0].value = "x".repeat(COMMAND_WORKING_BYTES / 4);
        op.execution = super::super::Execution::Command(Default::default());
        crate::omci_auth::test_authorize(&op.device.optical_sn);
        c.operations.insert("child".into(), op);
        c.admit_commands(&BTreeSet::from(["child".into()]), 100, |_| {
            panic!("oversized effect allocated")
        });
        assert_eq!(c.operations["child"].stage, OperationPhase::Completed);
        assert!(!c.previous_facts("child").unwrap().unresolved);
    }

    #[tokio::test]
    async fn byte_budget_limits_admission_before_the_effect_count_limit() {
        let mut c = Controller::default();
        let mut reachable = BTreeSet::new();
        for n in 0..3 {
            let mut op = super::super::tests::operation(OperationPhase::Waiting, false);
            op.device.id = format!("child{n}");
            op.device.provisioning_adapter = "hl202-mqtt".into();
            op.device.items[0].parameters[0].value = "x".repeat(1536 * 1024);
            op.execution = super::super::Execution::Command(Default::default());
            crate::omci_auth::test_authorize(&op.device.optical_sn);
            reachable.insert(op.device.id.clone());
            c.operations.insert(op.device.id.clone(), op);
        }
        let mut launched = 0;
        c.admit_commands(&reachable, 100, |_| {
            launched += 1;
            Job::test_spawn(async { Ok(json!({"items":[]})) })
        });
        assert_eq!(launched, 1);
        assert!(
            c.operations
                .values()
                .map(|op| op.command().map_or(0, |state| state.reservation))
                .sum::<usize>()
                <= COMMAND_WORKING_BYTES
        );
    }
}
