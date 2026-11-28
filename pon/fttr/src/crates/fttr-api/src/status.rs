//! Owned status wire models. Nullable measurements never imply observed zero.
use serde::{Deserialize, Serialize};

/// Board-specific register readback, not proof of optical error correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct PonMacStatus {
    pub desired_tx_enabled: bool,
    pub desired_fec_enabled: bool,
    pub register: Option<u32>,
    pub tx_enabled: Option<bool>,
    pub fec_enabled: Option<bool>,
    pub error: Option<String>,
}
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MeasurementEvidence {
    pub source: String,
    pub unit: String,
    pub reason: Option<String>,
    pub raw: Option<String>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub age_seconds: Option<u64>,
}
pub type Measurements = BTreeMap<String, MeasurementEvidence>;

/// Completed 15-minute downstream PM interval, not lifetime or upstream counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FecPmCounters {
    pub entity_id: u16,
    pub interval_end_time: u8,
    pub corrected_bytes: u32,
    pub corrected_code_words: u32,
    pub uncorrectable_code_words: u32,
    pub total_code_words: u32,
    pub fec_seconds: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FecStatus {
    pub ani_entity_id: Option<u16>,
    pub control_supported: bool,
    pub downstream_enabled: Option<bool>,
    pub upstream_enabled: Option<bool>,
    pub downstream_pm: Option<FecPmCounters>,
    pub measurement: MeasurementEvidence,
}

impl MeasurementEvidence {
    pub fn unavailable(source: impl Into<String>, unit: &str, reason: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            unit: unit.into(),
            reason: Some(reason.into()),
            raw: None,
            age_seconds: None,
        }
    }
    pub fn observed(source: impl Into<String>, unit: &str, raw: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            unit: unit.into(),
            reason: None,
            raw: Some(raw.into()),
            age_seconds: Some(0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalTransceiverStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub calibration_sha256: Option<String>,
    #[serde(default)]
    pub calibration_applied: bool,
    pub model: String,
    pub wavelength_tx_nm: u32,
    pub wavelength_rx_nm: u32,
    pub phy_rate_downlink_gbps: f32,
    pub phy_rate_uplink_gbps: f32,
    pub tx_power_dbm: Option<f32>,
    pub laser_bias_current_ma: Option<f32>,
    pub temperature_celsius: Option<f32>,
    pub vcc_voltage: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub tx_enabled: Option<bool>,
    pub measurements: Measurements,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct SwitchPortStatus {
    pub port: u8,
    pub interface: String,
    pub carrier: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub onu_id: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub onu_sn: Option<String>,
    pub link_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub reserved_sn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MeshClientStatus {
    pub mac: String,
    pub ip: String,
    pub band: String,
    pub rssi_dbm: Option<i32>,
    pub rx_rate_mbps: Option<u32>,
    pub tx_rate_mbps: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct DataPathOffloadStatus {
    pub enabled: Option<bool>,
    pub routed_enabled: Option<bool>,
    pub bridge_enabled: Option<bool>,
    pub engine_attached: Option<bool>,
    pub forwarding_verified: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuOmciInventory {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub age_seconds: u64,
    pub gem_ports: Vec<OnuGemPort>,
    pub vlan_filters: Vec<OnuVlanFilter>,
    pub vlan_operations: Vec<OnuVlanOperation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuGemPort {
    pub entity_id: u16,
    pub port_id: Option<u16>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuVlanFilter {
    pub entity_id: u16,
    pub filter_list_hex: Option<String>,
    pub forward_operation: Option<u32>,
    pub number_of_entries: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuVlanOperation {
    pub entity_id: u16,
    pub received_frame_rules_hex: Option<Vec<String>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct DataPathStatus {
    pub interface: String,
    pub gem_ports: Option<Vec<u16>>,
    pub vlan_id: Option<u16>,
    pub onu_inventory: Option<OnuOmciInventory>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub tx_bytes: Option<u64>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub rx_bytes: Option<u64>,
    pub current_tx_kbps: Option<f64>,
    pub current_rx_kbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub olt_channel_gem: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub forwarding_verified: Option<bool>,
    pub offload: DataPathOffloadStatus,
    pub measurements: Measurements,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct WifiMeshStatus {
    pub channel_2g: Option<u8>,
    pub channel_5g: Option<u8>,
    pub bandwidth_5g: Option<String>,
    pub tx_power_pct: Option<u8>,
    pub measurements: Measurements,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuIdentity {
    pub equipment_id: Option<String>,
    pub hardware_version: Option<String>,
    pub software_images: Vec<String>,
    pub omcc_version: Option<u8>,
    pub errors: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum RegistrationState {
    O1Initial,
    O2Standby,
    O3SerialNumber,
    O4Ranging,
    O5Operation,
    #[serde(rename = "o6_intermittent_lods")]
    O6IntermittentLODS,
    O7EmergencyStop,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum BusinessPhase {
    #[default]
    NotStarted,
    CollectingIdentity,
    RetryingIdentity,
    Unsupported,
    Configuring,
    Verifying,
    WanBootstrapPending,
    Verified,
    Retrying,
    Offline,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct BusinessStatus {
    pub phase: BusinessPhase,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub generation: u64,
    pub error: Option<String>,
}

impl std::fmt::Display for BusinessPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotStarted => "Not started",
            Self::CollectingIdentity => "Collecting identity",
            Self::RetryingIdentity => "Retrying identity",
            Self::Unsupported => "Adapter not compiled or unsupported device",
            Self::Configuring => "Configuring",
            Self::Verifying => "Verifying",
            Self::WanBootstrapPending => "WAN bootstrap pending",
            Self::Verified => "Verified",
            Self::Retrying => "Retrying",
            Self::Offline => "Offline",
        })
    }
}
impl std::fmt::Display for BusinessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (generation {})", self.phase, self.generation)?;
        if let Some(error) = &self.error {
            write!(f, ": {error}")?;
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct SubGatewayStatus {
    pub onu_id: u8,
    pub vendor: String,
    pub model: String,
    pub serial_number: String,
    pub state: RegistrationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub pon_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub upstream_phy_rate_gbps: Option<f32>,
    pub business_status: BusinessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub omci_auth: Option<crate::OmciAuthStatus>,
    pub management: Option<crate::ManagementStatus>,
    pub provisioning: Option<crate::ProvisioningStatus>,
    pub services: Vec<crate::SyncItem>,
    pub ssids: Vec<crate::SyncItem>,
    pub wifi_sync: crate::WifiSync,
    pub identity: Option<OnuIdentity>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub identity_age_seconds: Option<u64>,
    pub identity_current: bool,
    pub fiber_distance_m: Option<f32>,
    pub ranging_delay_rtd_ns: Option<u32>,
    pub equalization_delay_bits: Option<u32>,
    pub optical_rx_power_dbm: Option<f64>,
    pub optical_tx_power_dbm: Option<f64>,
    pub firmware_version: String,
    pub hardware_version: String,
    pub uptime_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub fec: Option<FecStatus>,
    pub data_path: DataPathStatus,
    pub wifi_mesh: WifiMeshStatus,
    pub connected_clients: Option<Vec<MeshClientStatus>>,
    pub measurements: Measurements,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct TelemetryGatewayInfo {
    pub id: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub last_seen_sec_ago: u64,
    pub last_payload_bytes: usize,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OnuPinning {
    pub desired: BTreeMap<String, u8>,
    pub active: BTreeMap<String, u8>,
    pub pending_restart: bool,
    pub allocation_blocked: bool,
    pub startup_error: Option<String>,
    pub config_error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct IpcStats {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub accepted: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub rejected: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub read_errors: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub handled: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub max_intake_us: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub max_handler_us: u64,
}
