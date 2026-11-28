//! Strongly typed data models for H3C FTTR Micro-OLT telemetry and IPC.

use serde::{Deserialize, Serialize};

mod business;
pub use business::{BusinessPhase, BusinessStatus};

// Compatibility import paths; production and generated clients share one definition.
pub use fttr_api::{
    DataPathOffloadStatus, DataPathStatus, MeasurementEvidence, Measurements, MeshClientStatus,
    NpuStatus, OnuGemPort, OnuIdentity, OnuOmciInventory, OnuVlanFilter, OnuVlanOperation,
    OpticalTransceiverStatus, PpeFlowEntry, StatusReply as FttrSystemStatus, SubGatewayStatus,
    SwitchPortStatus, TelemetryGatewayInfo, WifiMeshStatus,
};

/// Generic IPC response structure for control commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onu_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omci_messages: Option<usize>,
}

impl IpcResponse {
    pub fn ok(status: impl Into<String>) -> Self {
        Self {
            status: Some(status.into()),
            error: None,
            sn: None,
            action: None,
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self {
            status: None,
            error: Some(error.into()),
            sn: None,
            action: None,
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn subdev(
        status: impl Into<String>,
        sn: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            status: Some(status.into()),
            error: None,
            sn: Some(sn.into()),
            action: Some(action.into()),
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn to_json_line(&self) -> String {
        let mut s = serde_json::to_string(self)
            .unwrap_or_else(|_| "{\"error\":\"json serialize error\"}".to_string());
        s.push('\n');
        s
    }
}
