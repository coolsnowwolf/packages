use super::{error, io, json, Server, State, Task, TaskState, Value};

pub(super) struct PreparedRpc {
    pub(super) xml: String,
    pub(super) undo: DispatchUndo,
}

pub(super) struct DispatchUndo {
    pub(super) id: u64,
    pub(super) sent: Option<u64>,
}

impl DispatchUndo {
    pub(super) fn restore(self, state: &mut State) {
        #[expect(
            clippy::expect_used,
            reason = "the state lock prevents removal of the just-prepared task"
        )]
        let task = state
            .tasks
            .get_mut(&self.id)
            .expect("prepared task remains locked");
        task.state = TaskState::Queued;
        task.sent = self.sent;
        state.intents.remove(&self.id);
        state.release_dispatch(self.id);
    }
}

impl Server {
    pub(super) fn enqueue(&self, state: &mut State, task: Task) -> Value {
        self.enqueue_owned(state, task, false)
    }
    pub(super) fn enqueue_owned(&self, state: &mut State, task: Task, protected: bool) -> Value {
        state.prune();
        let argument_bytes: usize = state
            .tasks
            .values()
            .filter(|t| t.state.is_active())
            .map(|t| super::resources::memory(&t.args))
            .sum();
        if argument_bytes + super::resources::memory(&task.args) > super::resources::ARGUMENT_BYTES
            || state
                .tasks
                .values()
                .filter(|t| t.device == task.device && t.state.is_active())
                .count()
                >= 4
        {
            return json!({"error":"task ingress capacity exhausted", "code":"backpressure", "retryable":true});
        }
        let mut evicted = Vec::new();
        while state.tasks.len() >= 256 {
            let oldest = state
                .tasks
                .iter()
                .find(|(id, task)| {
                    !task.state.is_active()
                        && !task.unresolved_write()
                        && !state.results.get(id).is_some_and(|i| i.protected)
                })
                .map(|(id, _)| *id);
            let Some(id) = oldest else {
                state.tasks.extend(evicted);
                return json!({"error":"ACS task history full", "code":"backpressure", "retryable":true});
            };
            if let Some(task) = state.tasks.remove(&id) {
                evicted.push((id, task));
            }
        }
        let previous_id = state.next_id;
        let id = task.id;
        state.next_id = id;
        state.tasks.insert(id, task);
        if protected {
            state
                .results
                .insert(id, super::retention::ResultInfo::protected());
        }
        match self.save(state) {
            Ok(()) => {
                super::connection_request::wake();
                crate::provisioning::controller::wake_device(&state.tasks[&id].device);
                json!(state.tasks[&id])
            }
            Err(err) => {
                state.tasks.remove(&id);
                state.results.remove(&id);
                state.tasks.extend(evicted);
                state.next_id = previous_id;
                error(&format!("cannot persist queued task: {err}"))
            }
        }
    }

    pub(super) fn reset_tasks(&self, state: &mut State, device: &str) -> io::Result<()> {
        let tasks: Vec<_> = state
            .tasks
            .extract_if(.., |_, task| task.device == device)
            .collect();
        let results: Vec<_> = state
            .results
            .extract_if(.., |id, _| tasks.iter().any(|(task_id, _)| id == task_id))
            .collect();
        let intents: Vec<_> = state
            .intents
            .extract_if(.., |id, _| tasks.iter().any(|(task_id, _)| id == task_id))
            .collect();
        let sessions: Vec<_> = state
            .sessions
            .extract_if(.., |_, (id, _, _)| id == device)
            .collect();
        match self.save(state) {
            Ok(()) => {
                for (id, _) in &tasks {
                    state.release_dispatch(*id);
                }
                Ok(())
            }
            Err(err) => {
                state.tasks.extend(tasks);
                state.results.extend(results);
                state.intents.extend(intents);
                state.sessions.extend(sessions);
                Err(err)
            }
        }
    }
}
