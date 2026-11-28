//! Wire contracts shared by the native ubus server and the host-side SDK generator.
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

pub const OBJECT: &str = "fttr";

mod queries;
pub use queries::*;
mod fpga;
pub use fpga::*;
mod pon_stats;
pub use pon_stats::*;
mod hl202;
mod provisioning;
mod results;
pub use hl202::*;
mod status;
pub use provisioning::*;
pub use results::*;
pub use status::*;
mod commands;
mod operations;
pub use commands::*;
pub use operations::*;
mod topology;
pub use topology::*;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AdapterCapability {
    pub id: &'static str,
    pub name: &'static str,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AdapterCapabilities {
    pub omci: Vec<AdapterCapability>,
    pub bootstrap: Vec<AdapterCapability>,
    pub provisioning: Vec<AdapterCapability>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct Capabilities {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    pub schema_version: u32,
    pub version: &'static str,
    pub features: CapabilityFeatures,
    pub functions: CapabilityFunctions,
    pub pages: CapabilityPages,
    pub adapters: AdapterCapabilities,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CapabilityFeatures {
    pub acs: bool,
    #[serde(rename = "acs-bridge")]
    pub acs_bridge: bool,
    pub mqtt: bool,
    #[serde(rename = "mqtt-bridge")]
    pub mqtt_bridge: bool,
    pub npu: bool,
    #[serde(rename = "device-hl202")]
    pub device_hl202: bool,
    #[serde(rename = "device-hwtc")]
    pub device_hwtc: bool,
    #[serde(rename = "device-star")]
    pub device_star: bool,
    #[serde(rename = "device-zte")]
    pub device_zte: bool,
    #[serde(rename = "device-scty")]
    pub device_scty: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CapabilityFunctions {
    pub optical: bool,
    pub bridge_offload: bool,
    pub omci_mib: bool,
    pub mqtt_telemetry: bool,
    pub hl202_management: bool,
    pub cwmp: bool,
    pub command_provisioning: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CapabilityPages {
    pub status: bool,
    pub config: bool,
    pub sync: bool,
    pub hardware: bool,
    pub calibration: bool,
    pub hl202: bool,
    pub about: bool,
}

impl CapabilityPages {
    pub fn entries(self) -> [(&'static str, bool); 7] {
        [
            ("status", self.status),
            ("config", self.config),
            ("sync", self.sync),
            ("hardware", self.hardware),
            ("calibration", self.calibration),
            ("hl202", self.hl202),
            ("about", self.about),
        ]
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub settings: Option<OpticalSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub pon_mac: Option<PonMacStatus>,
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    #[serde(deserialize_with = "crate::results::version_one")]
    pub schema_version: u8,
    pub status: ReplyStatus,
    pub action: OpticalAction,
    pub desired_enabled: Option<bool>,
    pub tx_enabled: Option<bool>,
    pub actor_state: OpticalState,
    pub hardware_ready: bool,
    pub discovery_active: bool,
    pub cleanup_pending: bool,
    pub error: Option<OpticalError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReplyStatus {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OpticalAction {
    OpticalOn,
    OpticalOff,
    OpticalStatus,
    OpticalReload,
    OpticalReinitialize,
    OpticalShutdown,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OpticalState {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct EmptyRequest {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OpticalSettings {
    pub enabled: bool,
    pub downstream_tx_enabled: bool,
    pub fec_enabled: bool,
}

pub type OpticalSetRequest = OpticalSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Always,
    Npu,
    Hl202,
}

pub struct Method<Request, Response> {
    pub name: &'static str,
    pub sdk_name: &'static str,
    pub params: &'static [&'static str],
    pub access: Access,
    pub availability: Availability,
    types: PhantomData<fn(Request) -> Response>,
}

impl<Q, R> Method<Q, R> {
    pub const fn new(
        name: &'static str,
        sdk_name: &'static str,
        params: &'static [&'static str],
        access: Access,
        availability: Availability,
    ) -> Self {
        Self {
            name,
            sdk_name,
            params,
            access,
            availability,
            types: PhantomData,
        }
    }
}

// This list is consumed by both registration and code generation. Adding an entry
// also requires a typed handler at the server's registration site.
#[macro_export]
macro_rules! fttr_methods {
    ($consumer:ident) => {
        $consumer! {
            GET_CAPABILITIES, get_capabilities, getCapabilities, EmptyRequest, Capabilities, Read, Always, [];
            GET_PLATFORM_INFO, get_platform_info, getPlatformInfo, EmptyRequest, PlatformInfoReply, Read, Always, [];
            FPGA_DIAGNOSTIC, fpga_diagnostic, fpgaDiagnostic, FpgaDiagnosticRequest, FpgaDiagnosticReply, Read, Always, [onu_id];
            OPTICAL_STATUS, optical_status, opticalStatus, EmptyRequest, OpticalReply, Read, Always, [];
            OPTICAL_SET, optical_set, opticalSet, OpticalSettings, OpticalReply, Write, Always, [enabled, downstream_tx_enabled, fec_enabled];
            GET_STATUS, get_status, getStatus, EmptyRequest, StatusReply, Read, Always, [];
            GET_TOPOLOGY, get_topology, getTopology, EmptyRequest, TopologyReply, Read, Always, [];
            GET_TOPOLOGY_NODE, get_topology_node, getTopologyNode, TopologyNodeRequest, TopologyNodeReply, Read, Always, [node_id, snapshot_id];
            GET_TOPOLOGY_CLIENTS, get_topology_clients, getTopologyClients, TopologyClientsRequest, TopologyClientsReply, Read, Always, [node_id, snapshot_id, cursor, limit];
            GET_SYNC, get_sync, getSync, EmptyRequest, SyncStatusReply, Read, Always, [];
            CONFIG_CHECK, config_check, configCheck, EmptyRequest, ConfigCheckReply, Read, Always, [];
            NPU_STATUS, npu_status, npuStatus, EmptyRequest, NpuStatus, Read, Npu, [];
            CONFIG_RELOAD, config_reload, configReload, EmptyRequest, ConfigReloadReply, Write, Always, [];
            RESTART_SERVICE, restart_service, restartService, EmptyRequest, RestartReply, Write, Always, [];
            SYNC, sync, sync, SyncRequest, SyncReply, Write, Always, [action, target, management, item];
            NPU_SET, npu_set, npuSet, NpuSetRequest, NpuSetReply, Write, Npu, [enabled, bridge, wan_upstream, wan_downstream, onu_bridge];
            MIB_START, mib_start, mibStart, MibStartRequest, JobStartReply, Read, Always, [onu_id, optical_sn, tables];
            MIB_RESULT, mib_result, mibResult, JobRequest, JobChunkReply, Read, Always, [job, chunk];
            JOB_RELEASE, job_release, jobRelease, JobReleaseRequest, JobReleaseReply, Read, Always, [job];
            HL202_LIST, hl202_list, hl202List, EmptyRequest, HlListReply, Read, Hl202, [];
            HL202_REPORT, hl202_report, hl202Report, HlReportRequest, JobStartReply, Read, Hl202, [target];
            HL202_ACCOUNTS_START, hl202_accounts_start, hl202AccountsStart, HlAccountsRequest, JobStartReply, Read, Hl202, [target];
            HL202_ACCOUNT_UPDATE, hl202_account_update, hl202AccountUpdate, HlUpdateRequest, JobStartReply, Write, Hl202, [target, kind, entry, password, active];
            HL202_EXEC_START, hl202_exec_start, hl202ExecStart, HlExecRequest, JobStartReply, Write, Hl202, [target, script, timeout_seconds];
            JOB_RESULT, job_result, jobResult, JobRequest, JobChunkReply, Read, Always, [job, chunk];
            GET_CALIBRATION, get_calibration, getCalibration, EmptyRequest, CalibrationInfoReply, Read, Always, [];
            SELECT_CALIBRATION, select_calibration, selectCalibration, CalibrationSelectRequest, CalibrationInfoReply, Write, Always, [source];
            SAVE_CALIBRATION, save_calibration, saveCalibration, CalibrationSaveRequest, CalibrationSaveReply, Write, Always, [data];
            APPLY_CALIBRATION, apply_calibration, applyCalibration, EmptyRequest, JobStartReply, Write, Always, [];
        }
    };
}

macro_rules! declare_methods {
    ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {
        $(pub const $id: Method<$req, $res> = Method::new(stringify!($wire), stringify!($sdk), &[$(stringify!($param)),*], Access::$access, Availability::$availability);)*
    };
}
fttr_methods!(declare_methods);
