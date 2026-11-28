use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct PlatformInfoReply {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    pub schema_version: u8,
    pub selection: PlatformSelection,
    pub platform_id: String,
    pub vendor: String,
    pub model: String,
    pub onus: Vec<PlatformOnuResource>,
    pub operations: PlatformOperations,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlatformSelection {
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct PlatformOnuResource {
    pub onu_id: u8,
    pub interface: Option<String>,
}

/// Runtime operations advertised by the selected platform backend, not build features.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct PlatformOperations {
    pub optical_control: bool,
    pub transceiver_telemetry: bool,
    pub fpga_diagnostics: bool,
    pub bosa_diagnostics: bool,
    pub calibration_profile: Option<PlatformCalibrationProfile>,
    pub npu_offload: bool,
    pub omci_mib: bool,
    pub downstream_fec: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlatformCalibrationProfile {
    Ux3326,
    Ux3326sFactory,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct StatusReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub ubus: Option<std::collections::BTreeMap<String, crate::UbusMethodStats>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub jobs: Option<crate::JobStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub onu_pinning: Option<crate::OnuPinning>,
    pub fpga_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub pon_mac: Option<crate::PonMacStatus>,
    pub provisioning: Option<SyncStatusReply>,
    pub optical_transceiver: crate::OpticalTransceiverStatus,
    pub switch_ports: Vec<crate::SwitchPortStatus>,
    pub sub_gateways: Vec<crate::SubGatewayStatus>,
    pub telemetry_gateways: Vec<crate::TelemetryGatewayInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub npu: Option<NpuStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub ipc: Option<crate::IpcStats>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct SyncStatusReply {
    pub enabled: bool,
    pub mode: SyncMode,
    pub method: SyncMethod,
    pub revision: String,
    pub triggers: Vec<String>,
    // JSON numbers, not JS bigint. Exact large counters use separate strings.
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub interval_minutes: u64,
    pub error: Option<String>,
    pub devices: Vec<crate::DeviceSync>,
    pub management_network: crate::ManagementNetwork,
    pub omci_management: crate::OmciManagement,
    pub groups: Vec<SyncGroup>,
    pub pending_devices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub onu_pins: Option<std::collections::BTreeMap<String, u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub config_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub config_load: Option<ConfigLoadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub mqtt_bridge: Option<crate::MqttBridgeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub acs_bridge: Option<crate::AcsBridgeStatus>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct ConfigLoadStatus {
    pub state: ConfigLoadState,
    pub requested_revision: String,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub requested_at: Option<u64>,
    pub blocking_operations: Vec<ConfigLoadBlocker>,
    pub reset_pending: bool,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ConfigLoadState {
    Waiting,
    Applied,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct ConfigLoadBlocker {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub operation_id: u64,
    pub device: String,
    pub state: crate::OperationPhase,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub started_at: u64,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Auto,
    Manual,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncMethod {
    Diff,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct SyncGroup {
    pub id: String,
    pub name: Option<String>,
    pub template: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged, deny_unknown_fields)]
pub enum ConfigCheckReply {
    Valid {
        #[serde(deserialize_with = "crate::commands::true_value")]
        #[cfg_attr(feature = "codegen", ts(type = "true"))]
        valid: bool,
        revision: String,
        devices: usize,
    },
    Invalid {
        error: String,
    },
}

/// JSON numeric counters retain their existing wire encoding; exact values are
/// carried in *_exact strings for values beyond JavaScript's safe integer range.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct PpeFlowEntry {
    pub index: String,
    pub state: String,
    pub pkt_type: String,
    pub orig_tuple: String,
    pub new_tuple: String,
    pub eth: String,
    pub vlan: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub packets: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub bytes: u64,
    pub counters_valid: bool,
    pub packets_exact: String,
    pub bytes_exact: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum NpuAdmissionModel {
    PortMask,
    DirectionOnly,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct NpuStatus {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub sampled_at_ms: u64,
    pub errors: Vec<String>,
    pub config_valid: bool,
    pub engine_valid: bool,
    pub mask_valid: bool,
    pub routed_mask_valid: bool,
    pub debug_valid: bool,
    pub flows_valid: bool,
    pub entries_valid: bool,
    pub counters_valid: bool,
    pub hooks_valid: bool,
    pub total_packets_exact: String,
    pub total_bytes_exact: String,
    pub supported: bool,
    pub engine_attached: bool,
    pub bridge_offload_enabled: bool,
    pub wan_upstream: bool,
    pub wan_downstream: bool,
    pub onu_bridge: bool,
    pub bridge_offload_mask: u16,
    pub routed_offload_mask: u16,
    pub bridge_offload_debug: bool,
    pub bridge: String,
    pub active_ports: Vec<String>,
    pub fw4_hooks_ready: bool,
    pub total_entries: u32,
    pub bound_entries: u32,
    #[serde(default)]
    pub bound_ipv4_entries: u32,
    #[serde(default)]
    pub bound_ipv6_entries: u32,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub total_packets: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub total_bytes: u64,
    pub bound_flows: Vec<PpeFlowEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub admission_model: Option<NpuAdmissionModel>,
    #[serde(default)]
    pub direction_mask: u8,
    #[serde(default)]
    pub direction_mask_valid: bool,
}
