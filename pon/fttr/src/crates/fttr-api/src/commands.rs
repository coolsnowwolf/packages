use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::SyncStatusReply;

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CommandError {
    pub error: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum ConfigReloadReply {
    Reloaded(SyncStatusReply),
    Error(CommandError),
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct RestartReply {
    #[serde(deserialize_with = "crate::commands::true_value")]
    #[cfg_attr(feature = "codegen", ts(type = "true"))]
    pub restarting: bool,
}

pub(crate) fn true_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    if bool::deserialize(deserializer)? {
        Ok(true)
    } else {
        Err(serde::de::Error::custom("expected true"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Plan,
    Apply,
    ManagementPlan,
    ManagementApply,
    ResetState,
    Reconcile,
    Retry,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct SyncRequest {
    pub action: SyncAction,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub management: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub item: Option<String>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

impl SyncRequest {
    /// Preserve provisioning operation names and selectors without shell/CLI translation.
    pub fn provisioning_request(&self) -> Result<Value, &'static str> {
        if !identifier(self.target.strip_prefix("group:").unwrap_or(&self.target)) {
            return Err("invalid synchronization target");
        }
        Ok(json!({
            "op": self.action,
            "target": self.target,
            "management": self.management.unwrap_or(false),
            "item": self.item,
        }))
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy)]
pub enum OperationMode {
    Plan,
    Apply,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum SyncReply {
    Selected {
        #[cfg_attr(feature = "codegen", ts(type = "Array<number>"))]
        operation_ids: Vec<u64>,
        revision: String,
    },
    Management {
        #[serde(deserialize_with = "true_value")]
        #[cfg_attr(feature = "codegen", ts(type = "true"))]
        started: bool,
        mode: OperationMode,
    },
    Reset {
        #[serde(deserialize_with = "true_value")]
        #[cfg_attr(feature = "codegen", ts(type = "true"))]
        reset: bool,
        device: String,
    },
    Triggered {
        #[cfg_attr(feature = "codegen", ts(type = "Array<number>"))]
        operation_ids: Vec<u64>,
        mode: OperationMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "codegen", ts(optional))]
        reconcile: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "codegen", ts(optional))]
        retry: Option<bool>,
    },
    Error(CommandError),
}
