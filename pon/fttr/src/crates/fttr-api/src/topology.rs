//! Read-only topology projections. Only implemented observations belong here.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyNodeRequest {
    pub node_id: String,
    #[serde(default)]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyClientsRequest {
    pub node_id: String,
    #[serde(default)]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub snapshot_id: Option<String>,
    /// Opaque cursor returned by the preceding page; requires snapshot_id.
    #[serde(default)]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub cursor: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologySource {
    pub id: String,
    pub state: TopologySourceState,
    /// Unix time at which this source was sampled (not a promise of atomicity).
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub sampled_at: u64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologySourceState {
    Ok,
    Partial,
    Unavailable,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyIssue {
    pub code: String,
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyReply {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    pub schema_version: u8,
    pub snapshot_id: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub generated_at: u64,
    pub snapshot_ttl_seconds: u32,
    pub nodes: Vec<TopologyNode>,
    pub links: Vec<TopologyLink>,
    pub sources: Vec<TopologySource>,
    pub issues: Vec<TopologyIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyNode {
    pub id: String,
    pub label: String,
    pub detail: TopologyNodeDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyNodeDetail {
    Controller(TopologyController),
    Onu(TopologyOnu),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyController {
    pub model: Option<String>,
    pub firmware_version: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub fpga_status: String,
    pub optical: TopologyOpticalModule,
    pub interfaces: Vec<TopologyInterface>,
    /// Interfaces with an observed default route; not Internet reachability.
    pub uplinks: Vec<TopologyUplink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyOpticalModule {
    pub model: String,
    pub wavelength_tx_nm: u32,
    pub wavelength_rx_nm: u32,
    /// Shared nominal PON capacity, not per-ONU negotiated or measured speed.
    pub nominal_downlink_gbps: f32,
    pub nominal_uplink_gbps: f32,
    pub tx_power_dbm: Option<f32>,
    pub laser_bias_current_ma: Option<f32>,
    pub temperature_celsius: Option<f32>,
    pub vcc_voltage: Option<f32>,
    pub tx_enabled: Option<bool>,
    pub measurements: Measurements,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyInterface {
    pub name: String,
    pub mac: Option<String>,
    pub carrier: Option<bool>,
    /// Only physical interfaces; unknown/negative speeds are null.
    pub speed_mbps: Option<u32>,
    pub duplex: Option<String>,
    pub bridge_members: Vec<String>,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyUplink {
    pub interface: String,
    pub device: Option<String>,
    pub protocol: Option<String>,
    pub up: bool,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyOnu {
    pub serial_number: Option<String>,
    pub onu_id: Option<u8>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub hardware_version: Option<String>,
    pub firmware_version: Option<String>,
    /// None means a binding-only placeholder, not an invented O1 observation.
    pub registration: Option<RegistrationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub pon_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub upstream_phy_rate_gbps: Option<f32>,
    pub business: Option<BusinessStatus>,
    pub management: Option<ManagementStatus>,
    pub provisioning: Option<ProvisioningStatus>,
    pub services: Vec<SyncItem>,
    pub ssids: Vec<SyncItem>,
    pub wifi_sync: Option<WifiSync>,
    pub identity: Option<OnuIdentity>,
    pub identity_current: bool,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub identity_age_seconds: Option<u64>,
    pub uptime_seconds: Option<f64>,
    pub fec: Option<FecStatus>,
    pub binding: TopologyBinding,
    pub extensions: TopologyExtensions,
    pub measurements: Measurements,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyBinding {
    pub active_onu_id: Option<u8>,
    pub desired_onu_id: Option<u8>,
    pub state: TopologyBindingState,
    pub pending_restart: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyBindingState {
    None,
    Effective,
    PendingAdd,
    PendingChange,
    PendingRemove,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyExtensions {
    pub hl202: Option<TopologyHl202>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyHl202 {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub report_revision: u64,
    pub binding_generation: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub age_seconds: u64,
    pub ip: Option<String>,
    pub bridge_ip: Option<String>,
    pub mac: Option<String>,
    pub connection_type_raw: Option<String>,
    pub uptime_seconds: Option<f64>,
    /// Unit/percentage semantics are not verified; preserve device integers.
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub cpu_usage_raw: Option<u64>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub memory_usage_raw: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation: TopologyRelation,
    /// A logical interface, never a claim about physical splitter/port layout.
    pub data_interface: Option<String>,
    pub carrier: Option<bool>,
    pub onu_rx_power_dbm: Option<f64>,
    pub onu_tx_power_dbm: Option<f64>,
    pub equalization_delay_bits: Option<u32>,
    pub data_path: Option<TopologyDataPath>,
    pub measurements: Measurements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyRelation {
    PonLogical,
    AssociatedWith,
    LearnedBehind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyDataPath {
    /// RX is ONU -> controller; TX is controller -> ONU.
    pub observer_node_id: String,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub rx_bytes: Option<u64>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub tx_bytes: Option<u64>,
    pub current_rx_kbps: Option<f64>,
    pub current_tx_kbps: Option<f64>,
    pub onu_inventory: Option<OnuOmciInventory>,
    pub offload: DataPathOffloadStatus,
    pub measurements: Measurements,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyNodeReply {
    Ok {
        snapshot_id: String,
        node: TopologyNode,
    },
    Error {
        code: TopologyQueryError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyQueryError {
    SnapshotExpired,
    NodeNotFound,
    InvalidCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TopologyClient {
    pub id: String,
    pub mac: String,
    pub hostname: Option<String>,
    pub addresses: Vec<String>,
    pub relation: TopologyRelation,
    pub source: String,
    pub interface: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum TopologyClientsReply {
    Ok {
        snapshot_id: String,
        parent_node_id: String,
        clients: Vec<TopologyClient>,
        /// None when that inventory was not successfully read.
        associated_client_count: Option<u32>,
        learned_host_count: Option<u32>,
        /// FDB/host hints cannot establish an exhaustive terminal inventory.
        complete: bool,
        next_cursor: Option<String>,
        sources: Vec<TopologySource>,
    },
    Error {
        code: TopologyQueryError,
    },
}
