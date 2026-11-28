//! HM2004 observed register diagnostics; counters are not standard ONU FEC PM.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FpgaDiagnosticRequest {
    #[serde(default)]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub onu_id: Option<u8>,
}
impl FpgaDiagnosticRequest {
    pub fn slots(&self) -> io::Result<Vec<u8>> {
        match self.onu_id {
            Some(id) if id < 16 => Ok(vec![id]),
            None => Ok((0..16).collect()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONU slot must be 0..15",
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaGrant {
    pub bandwidth_raw: u32,
    pub control_raw: u32,
    pub enabled_bit28: bool,
    /// Raw low 20 bits; physical units have not been established.
    pub bandwidth: u32,
    pub weight: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaOnu {
    pub slot: u8,
    /// Opaque registration token; never a serial number.
    pub generation: Option<String>,
    pub lob: u16,
    pub bip: u16,
    pub management: FpgaGrant,
    pub business: FpgaGrant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaCounterDescriptor {
    pub key: String,
    pub label: String,
    pub group: String,
    pub address: String,
    pub width_bits: u8,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaSnapshot {
    #[cfg_attr(feature = "codegen", ts(type = "2"))]
    pub schema_version: u8,
    pub source_sha256: String,
    pub limitations: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub started_unix_ms: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub read_duration_us: u64,
    /// Monotonic clock relative to this daemon's first diagnostic read.
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub started_monotonic_ms: Option<u64>,
    pub daemon_instance: Option<String>,
    pub hardware_epoch: Option<String>,
    pub main_bandwidth_raw: u32,
    pub main_bandwidth_enabled: bool,
    pub onus: Vec<FpgaOnu>,
    pub counters: BTreeMap<String, u32>,
    pub counter_descriptors: Vec<FpgaCounterDescriptor>,
    pub raw_registers: BTreeMap<String, u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum FpgaDiagnosticReply {
    Success { snapshot: FpgaSnapshot },
    Error { error: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaOnuDelta {
    pub slot: u8,
    pub registration_continuity_verified: bool,
    pub lob_modulo_65536: Option<u16>,
    pub bip_modulo_65536: Option<u16>,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FpgaDelta {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub elapsed_ms: u64,
    pub same_daemon: bool,
    pub hardware_continuity_verified: bool,
    pub onus: Vec<FpgaOnuDelta>,
    pub counters_modulo_4294967296: Option<BTreeMap<String, u32>>,
}
