//! Final asynchronous payloads, independent from job chunk transport envelopes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) fn version_one<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u8, D::Error> {
    match u8::deserialize(deserializer)? {
        1 => Ok(1),
        _ => Err(serde::de::Error::custom("unsupported schema version")),
    }
}

fn account_entry<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    match u8::deserialize(deserializer)? {
        entry @ 0..=2 => Ok(entry),
        _ => Err(serde::de::Error::custom("invalid account entry")),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibReference {
    pub id: String,
    pub url: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MibAttributeKind {
    Unknown,
    Octets,
    String,
    UnsignedInteger,
    Table,
    SignedInteger,
    Pointer,
    BitField,
    Enumeration,
    Counter,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibDefinition {
    pub name: String,
    pub kind: MibAttributeKind,
    pub index: u8,
    pub mask: u16,
    pub size: usize,
    pub read: bool,
    pub write: bool,
    pub set_by_create: bool,
    pub avc: bool,
    pub optional: bool,
    pub deprecated: bool,
    pub default_hex: Option<String>,
    pub reference: MibReference,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MibValue {
    Unsigned { value: u32 },
    Signed { value: i32 },
    Decimal { value: String },
    Text { value: Option<String> },
    Octets { hex: String },
    TableLength { bytes: u32 },
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibDiagnostic {
    pub code: String,
    pub message: String,
    pub offset: usize,
    pub mask: u16,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MibSupport {
    Layout,
    NameOnly,
    Unknown,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibVlanRule {
    pub raw_hex: String,
    pub filter_outer_priority: u8,
    pub filter_outer_vid: u16,
    pub filter_outer_tpid_dei: u8,
    pub filter_inner_priority: u8,
    pub filter_inner_vid: u16,
    pub filter_inner_tpid_dei: u8,
    pub filter_ethertype: u8,
    pub tags_to_remove: u8,
    pub treatment_outer_priority: u8,
    pub treatment_outer_vid: u16,
    pub treatment_outer_tpid_dei: u8,
    pub treatment_inner_priority: u8,
    pub treatment_inner_vid: u16,
    pub treatment_inner_tpid_dei: u8,
    pub delete_entry: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibObservation {
    pub raw_hex: String,
    pub value: MibValue,
    pub sequences: Vec<u16>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibAttribute {
    pub definition: MibDefinition,
    pub value: Option<MibValue>,
    pub observations: Vec<MibObservation>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibEntity {
    pub class_id: u16,
    pub entity_id: u16,
    pub name: Option<String>,
    pub support: MibSupport,
    pub reference: Option<MibReference>,
    pub sequences: Vec<u16>,
    pub attributes: Vec<MibAttribute>,
    pub diagnostics: Vec<MibDiagnostic>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibClass {
    pub class_id: u16,
    pub name: Option<String>,
    pub entities: usize,
    pub records: usize,
    pub support: MibSupport,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibRecord {
    pub class_id: u16,
    pub entity_id: u16,
    pub sequence: u16,
    pub attribute_mask: u16,
    pub raw_hex: String,
    pub consumed: usize,
    pub remaining_hex: String,
    pub undecoded_mask: u16,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibTable {
    pub class_id: u16,
    pub entity_id: u16,
    pub attribute: MibDefinition,
    pub collected: bool,
    pub error: Option<String>,
    pub raw_hex: String,
    pub rows: Vec<String>,
    pub vlan_rules: Vec<MibVlanRule>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibReport {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    #[serde(deserialize_with = "version_one")]
    pub schema_version: u8,
    pub reference_version: String,
    pub onu_id: Option<u8>,
    pub uploaded_complete: bool,
    pub expected_records: u16,
    pub received_records: usize,
    pub decoded_records: usize,
    pub partial_records: usize,
    pub basic_records: usize,
    pub tables_requested: bool,
    pub tables_collected: bool,
    pub classes: Vec<MibClass>,
    pub entities: Vec<MibEntity>,
    pub records: Vec<MibRecord>,
    pub tables: Vec<MibTable>,
    pub diagnostics: Vec<MibDiagnostic>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MibReady {
    Ready,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibSnapshot {
    pub status: MibReady,
    pub report: MibReport,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct HlTelemetryMember {
    pub name: String,
    pub bytes: usize,
    pub encoding: String,
    pub text: Option<String>,
    pub text_omitted: bool,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct HlTelemetryField {
    // Device-specific telemetry values are intentionally open JSON, unlike envelopes.
    #[cfg_attr(feature = "codegen", ts(type = "unknown"))]
    pub value: serde_json::Value,
    pub member: String,
    pub unit: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone)]
pub struct HlTelemetry {
    pub schema_version: u8,
    pub compressed_bytes: usize,
    pub decompressed_bytes: usize,
    pub fields: BTreeMap<String, HlTelemetryField>,
    pub members: Vec<HlTelemetryMember>,
    pub errors: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default)]
pub struct HlReportState {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub revision: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub last_received: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub last_processed_receipt: Option<u64>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub last_good: Option<u64>,
    pub error: Option<String>,
    pub report: Option<HlTelemetry>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HlReportPhase {
    Received,
    ReportUnavailable,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum HlReportReply {
    Report {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        #[serde(deserialize_with = "version_one")]
        schema_version: u8,
        target: crate::MqttSource,
        report: Option<HlReportState>,
        state: HlReportPhase,
    },
    Source {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        #[serde(deserialize_with = "version_one")]
        schema_version: u8,
        target: crate::MqttSource,
        report: Option<HlReportState>,
        #[cfg_attr(feature = "codegen", ts(type = "true"))]
        #[serde(deserialize_with = "crate::commands::true_value")]
        read_only: bool,
    },
    Error {
        schema_version: u8,
        error: crate::OpticalError,
    },
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlCommandStatus {
    Executed,
    Dispatched,
    Timeout,
    Error,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug)]
pub struct HlCommandResult {
    pub target: String,
    pub status: HlCommandStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub output: Option<String>,
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[derive(Debug)]
pub struct HlOperation {
    pub target: crate::MqttSource,
    pub result: HlCommandResult,
    pub binding_current: bool,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlAccount {
    #[cfg_attr(feature = "codegen", ts(type = "0 | 1 | 2"))]
    #[serde(deserialize_with = "account_entry")]
    pub entry: u8,
    pub username: String,
    pub active: bool,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HlWriteStatus {
    NotApplicable,
    AcceptedAndSaved,
    Unconfirmed,
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum HlAccountReply {
    Accounts {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        #[serde(deserialize_with = "version_one")]
        schema_version: u8,
        operation: HlOperation,
        accounts: Option<Vec<HlAccount>>,
        write_status: HlWriteStatus,
    },
    Error {
        schema_version: u8,
        error: crate::OpticalError,
    },
}
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum HlExecReply {
    Executed {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        #[serde(deserialize_with = "version_one")]
        schema_version: u8,
        operation: HlOperation,
    },
    Error {
        schema_version: u8,
        error: crate::OpticalError,
    },
}
