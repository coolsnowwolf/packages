//! Task records share response ownership with internal consumers.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) fn error_result(message: &str) -> super::resources::Response {
    super::resources::Response::uncharged(super::error(message))
}

pub(super) fn is_write(method: &str) -> bool {
    matches!(
        method,
        "SetParameterValues"
            | "AddObject"
            | "DeleteObject"
            | "Reboot"
            | "FactoryReset"
            | "Download"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskState {
    Queued,
    Sent,
    Completed,
    AcceptedPendingApply,
    Unknown,
    Fault,
    Expired,
}

impl TaskState {
    pub(super) fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Sent)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Task {
    pub(super) id: u64,
    pub(super) device: String,
    pub(super) method: String,
    pub(super) args: Value,
    pub(super) state: TaskState,
    pub(super) created: u64,
    pub(super) sent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<super::resources::Response>,
}

impl Task {
    pub(super) fn unresolved_write(&self) -> bool {
        is_write(&self.method)
            && matches!(
                self.state,
                TaskState::Unknown | TaskState::AcceptedPendingApply
            )
    }

    pub(super) fn checkpoint_write(&self) -> bool {
        self.unresolved_write() || (self.state == TaskState::Sent && is_write(&self.method))
    }

    pub(super) fn release_request(&mut self) {
        if !self.state.is_active() {
            // Raw requests may carry credentials. The result and its lease are
            // separate and remain available to the original consumer.
            self.args = Value::Null;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_states_round_trip() {
        for (state, expected_str) in [
            (TaskState::Queued, "queued"),
            (TaskState::Sent, "sent"),
            (TaskState::Completed, "completed"),
            (TaskState::AcceptedPendingApply, "accepted_pending_apply"),
            (TaskState::Unknown, "unknown"),
            (TaskState::Fault, "fault"),
            (TaskState::Expired, "expired"),
        ] {
            let serialized = serde_json::to_string(&state).unwrap();
            assert_eq!(serialized, format!("\"{expected_str}\""));
            let deserialized: TaskState = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, state);
            assert_eq!(
                state.is_active(),
                matches!(state, TaskState::Queued | TaskState::Sent)
            );
        }
    }
}
