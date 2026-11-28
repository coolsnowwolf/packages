//! Provisioning snapshots. Optional fields distinguish observations not yet made.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

wire_enum!(OperationPhase {
    Disabled => "disabled",
    Waiting => "waiting", WanReconciling => "wan_reconciling",
    WanVerifying => "wan_verifying", WifiReconciling => "wifi_reconciling",
    Discovering => "discovering", Reading => "reading", Disabling => "disabling",
    DisablingRead => "disabling_read", Writing => "writing", Verifying => "verifying",
    Settling => "settling", Completed => "completed", NotStarted => "not_started",
    ResetPending => "reset_pending", Interrupted => "interrupted", Recovering => "recovering",
    WaitingManagement => "waiting_management", WaitingTask => "waiting_task",
});
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, Default)]
pub enum WifiSync {
    Disabled,
    #[default]
    NotConfigured,
    StaleRevision,
    StaleManagement,
    Verified,
    AttentionRequired,
    AcceptedUnverified,
    Pending,
    Recovering,
    WaitingManagement,
}
wire_enum!(SyncItemKind {
    DeviceService => "device_service", Ssid => "ssid",
    PortBinding => "port_binding", WanActivation => "wan_activation",
});

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct SyncChange {
    pub name: String,
    pub desired: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct SyncItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub name: Option<String>,
    pub kind: SyncItemKind,
    pub service_type: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub current_revision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub management_fresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub operation_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number | null", optional = nullable))]
    pub task_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub changes: Option<Vec<SyncChange>>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct Lifecycle {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number | null", optional = nullable))]
    pub task_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub instance: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub cwmp_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementState {
    Disabled,
    Online,
    WaitingInform,
    OfflineOrStale,
    WaitingTransport,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct ManagementStatus {
    pub cwmp_id: String,
    pub state: ManagementState,
    pub optical_online: bool,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub last_inform: Option<u64>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub age_seconds: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MqttStatus {
    pub state: ManagementState,
    pub optical_online: bool,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct ManagementDesired {
    pub vlan: u32,
    pub vlans: Vec<u32>,
    pub priority: u32,
    pub acs_url: String,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AcsListener {
    pub enabled: bool,
    pub listen: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BootstrapPhase {
    #[default]
    WaitingTransport,
    WaitingIdentity,
    Reading,
    Configuring,
    Verifying,
    WaitingInform,
    Ready,
    RetryWait,
    ManualRequired,
    NotConfigured,
    WaitingAcs,
    AwaitingApply,
    DiscoveringManagementWan,
    ConfiguringManagementWan,
    VerifyingManagementWan,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct BootstrapSnapshot {
    pub state: BootstrapPhase,
    pub optical_sn: String,
    pub cwmp_id: String,
    pub mqtt_target: Option<String>,
    pub binding_source: String,
    pub method: String,
    pub desired_version: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub generation: u64,
    pub reason: String,
    pub attempts: u32,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub next_retry_at: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub updated_at: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub settings_verified_at: Option<u64>,
    pub selected_vlan: Option<u32>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub last_inform: Option<u64>,
    pub expected_acs_url: String,
    pub last_error: Option<String>,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub transport_epoch: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub completed_authorization: u64,
    pub connection_request_enabled: Option<bool>,
    pub inflight: bool,
    pub persistence_error: Option<String>,
    pub link_identity_error: Option<String>,
    pub listener: AcsListener,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub mqtt_last_seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub desired: Option<ManagementDesired>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub endpoint_notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub manual_steps: Option<Vec<String>>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BootstrapUnavailablePhase {
    NotRequired,
    Pending,
    Unavailable,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum AcsBootstrap {
    Snapshot(BootstrapSnapshot),
    Unavailable {
        state: BootstrapUnavailablePhase,
        reason: String,
    },
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct BackendStatus {
    pub configured: String,
    pub effective: String,
    pub available: Vec<String>,
    pub reason: String,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct CwmpCapabilities {
    pub wan: String,
    pub wlan: String,
    pub binding: String,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct CwmpContract {
    pub state: String,
    pub requested: String,
    pub contract: Option<String>,
    pub reason: String,
    pub cache_hit: bool,
    pub schema: String,
    pub capabilities: Option<CwmpCapabilities>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct WanOwned {
    pub parent: u32,
    pub connection: u32,
    pub state: String,
    pub service_list: String,
    pub kind: String,
    pub baseline: Option<Vec<u32>>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct WifiOwned {
    pub owned: Vec<u32>,
    pub pending: Option<String>,
    pub pending_instance: Option<u32>,
}
wire_enum!(WanStage {
    Waiting => "waiting", Inventory => "inventory", Classifying => "classifying",
    AddingParent => "adding_parent", ChildNames => "child_names", AddingChild => "adding_child",
    DeleteGuard => "delete_guard", Deleting => "deleting", VerifyDelete => "verify_delete",
    EmptyParent => "empty_parent", DeletingParent => "deleting_parent",
    VerifyParentDelete => "verify_parent_delete", Next => "next", Done => "done",
});
wire_enum!(WifiStage {
    Waiting => "waiting", Inventory => "inventory", Adding => "adding", VerifyAdd => "verify_add",
    Deleting => "deleting", VerifyDelete => "verify_delete", ReadingDisable => "reading_disable",
    Disabling => "disabling", VerifyingDisable => "verifying_disable", Next => "next", Done => "done",
});
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone)]
pub enum WifiCleanup {
    Disable,
    Preserve,
    Delete,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct ProvisioningStatus {
    pub state: OperationPhase,
    pub items: Vec<SyncItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub omci_adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub bootstrap_adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub provisioning_adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub backend: Option<BackendStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub operation_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub optical_sn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub cwmp_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub mode: Option<crate::OperationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number | null", optional = nullable))]
    pub completed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub current_revision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub management_fresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub next_retry_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub reconcile_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub cwmp_contract: Option<CwmpContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wan_objects: Option<BTreeMap<String, WanOwned>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wan_lifecycle: Option<BTreeMap<String, Lifecycle>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wan_stage: Option<WanStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wifi_objects: Option<WifiOwned>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wifi_lifecycle: Option<BTreeMap<String, Lifecycle>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wifi_stage: Option<WifiStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub wifi_cleanup: Option<WifiCleanup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub native_ownership: Option<BTreeMap<String, NativeWanOwned>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub write_performed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub readback_verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub recovery_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub error_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub manual_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number", optional))]
    pub last_verified_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub verified_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub publication_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub remote_execution_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub write_attempted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub terminal_received: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub write_verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub affected_items: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub automatic_write_admission: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct NativeWanOwned {
    pub parent: u32,
    pub child: u32,
    pub service: String,
    pub vlan: String,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct NetworkConfig {
    pub enabled: bool,
    pub mode: crate::SyncMode,
    pub vlan: Vec<u32>,
    pub priority: u32,
    pub address: String,
    pub acs_url: String,
    pub port: u16,
    pub path: String,
    pub dhcp_enabled: bool,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy)]
pub enum NetworkPhase {
    Discovering,
    Comparing,
    Planning,
    Applying,
    Verifying,
    ConfiguringListener,
    CleaningPrevious,
    WaitingDevices,
    Ready,
    AwaitingApply,
    Verified,
    Blocked,
    RetryWait,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct NetworkPlan {
    pub operation: String,
    pub key: String,
    pub value: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct ManagementNetwork {
    pub state: NetworkPhase,
    pub error: Option<String>,
    pub inflight: bool,
    pub ready: bool,
    pub desired_version: Option<String>,
    pub applied_version: Option<String>,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub next_retry_at: u64,
    pub attempts: u32,
    pub desired: Option<NetworkConfig>,
    pub applied: Option<NetworkConfig>,
    pub previous: Option<NetworkConfig>,
    pub plan: Vec<NetworkPlan>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OmciManagement {
    pub desired: Option<ManagementDesired>,
    pub state: String,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct DeviceOmciManagement {
    pub state: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub vlan: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional, type = "number"))]
    pub updated_at: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OmciAuthMode {
    Disabled,
    LoidPassword,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OmciAuthSource {
    Global,
    Device,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OmciAuthState {
    Pending,
    Checking,
    Verified,
    Skipped,
    Denied,
    Error,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OmciAuthStatus {
    pub mode: OmciAuthMode,
    pub source: OmciAuthSource,
    pub state: OmciAuthState,
    pub backend: Option<String>,
    /// Actual ONU-reported credentials, never the configured comparison values.
    pub reported_loid: Option<String>,
    pub reported_password: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MqttBridgeRule {
    pub interface: String,
    pub client_mac: String,
    pub client_ip: String,
    pub target_gw: String,
    pub broker_ip: String,
    pub bridge_mac: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub optical_sn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub binding_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub evidence_source: Option<String>,
    pub bridge_rule: String,
    pub ip_rule: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MqttBridgeStatus {
    pub enabled: bool,
    pub state: String,
    pub lan_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub broker_listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub upstream_gateway: Option<String>,
    pub active_rules: u32,
    #[serde(default)]
    pub rules: Vec<MqttBridgeRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AcsBridgeRule {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub name: Option<String>,
    pub scope: String,
    pub target_ip: String,
    pub target_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub path: Option<String>,
    pub path_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub optical_sn: Option<String>,
    #[serde(default)]
    pub interfaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub acs_target: Option<String>,
    #[serde(default)]
    pub bridge_rules: Vec<String>,
    #[serde(default)]
    pub ip_rules: Vec<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AcsBridgeStatus {
    pub enabled: bool,
    pub management_gateway: bool,
    pub state: String,
    pub lan_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub lan_listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub management_listen: Option<String>,
    pub path_mode: String,
    #[serde(default)]
    pub accepted_paths: Vec<String>,
    pub active_rules: u32,
    #[serde(default)]
    pub rules: Vec<AcsBridgeRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct DeviceMqttBridgeStatus {
    pub state: String,
    pub address_origin: String,
    pub mode: String,
    pub rule_installed: bool,
    pub telemetry_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub client_mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub client_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub target_broker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub binding_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub evidence_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number | null", optional = nullable))]
    pub evidence_updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(type = "number | null", optional = nullable))]
    pub mqtt_last_seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional = nullable))]
    pub reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct DeviceSync {
    pub device: String,
    pub name: Option<String>,
    pub group: Option<String>,
    pub group_name: Option<String>,
    pub template: Option<String>,
    pub template_name: Option<String>,
    pub optical_sn: String,
    pub omci_adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub omci_auth: Option<OmciAuthStatus>,
    pub bootstrap_adapter: String,
    pub provisioning_adapter: String,
    pub omci_management: DeviceOmciManagement,
    pub management: Option<ManagementStatus>,
    pub mqtt: Option<MqttStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub mqtt_bridge: Option<DeviceMqttBridgeStatus>,
    pub acs_bootstrap: AcsBootstrap,
    pub provisioning: ProvisioningStatus,
    pub services: Vec<SyncItem>,
    pub ssids: Vec<SyncItem>,
    pub wifi_sync: WifiSync,
}
