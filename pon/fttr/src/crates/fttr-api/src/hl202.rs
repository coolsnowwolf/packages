//! Shared HL202 selections and command protocol, used by ubus, IPC and adapters.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlOpticalSelection {
    pub optical_sn: String,
    pub onu_id: u16,
    pub binding_generation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlSourceSelection {
    pub mqtt_mac: String,
    pub binding_generation: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum HlAction {
    Exec {
        script: String,
        timeout_seconds: u16,
    },
    AccountsRead,
    AccountSetPassword {
        entry: u8,
        password: String,
    },
    AccountSetActive {
        entry: u8,
        active: bool,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlCommandRequest {
    pub schema_version: u8,
    pub target: HlOpticalSelection,
    pub action: HlAction,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum HlQuery {
    List,
    Report { target: HlOpticalSelection },
    SourceReport { target: HlSourceSelection },
    Run { request: HlCommandRequest },
}

impl std::fmt::Display for crate::HlCommandStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Executed => "executed",
            Self::Dispatched => "dispatched",
            Self::Timeout => "timeout",
        })
    }
}

impl crate::HlCommandResult {
    pub fn failed(target: &str, error: impl ToString) -> Self {
        Self {
            target: target.into(),
            status: crate::HlCommandStatus::Error,
            request_id: None,
            exit_code: None,
            output: None,
            output_truncated: false,
            error: Some(error.to_string()),
        }
    }
    pub fn success(&self) -> bool {
        self.error.is_none()
            && match self.status {
                crate::HlCommandStatus::Executed => self.exit_code == Some(0),
                crate::HlCommandStatus::Dispatched => self.exit_code.is_none(),
                crate::HlCommandStatus::Error | crate::HlCommandStatus::Timeout => false,
            }
    }
}

impl crate::HlReportState {
    /// Preserve the last good report when a newer receipt fails decoding.
    pub fn apply(&mut self, received: u64, result: Result<crate::HlTelemetry, String>) {
        self.revision = self.revision.wrapping_add(1);
        self.last_received = self.last_received.max(received);
        self.last_processed_receipt = Some(received);
        match result {
            Ok(report) => {
                self.last_good = Some(received);
                self.report = Some(report);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}
