//! Internal business models. Serialized names are part of the existing status API.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant),+
        }

        impl $name {
            pub(super) const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

/// Scheduler observations share immutable desired data. Mutating a reload or
/// resolving an identity detaches it, so an older observer keeps its generation.
#[derive(Clone, Default)]
pub(super) struct SharedConfig(std::sync::Arc<Config>);
impl std::ops::Deref for SharedConfig {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.0
    }
}
impl std::ops::DerefMut for SharedConfig {
    fn deref_mut(&mut self) -> &mut Config {
        std::sync::Arc::make_mut(&mut self.0)
    }
}
impl From<Config> for SharedConfig {
    fn from(config: Config) -> Self {
        Self(std::sync::Arc::new(config))
    }
}
impl From<SharedConfig> for Config {
    fn from(config: SharedConfig) -> Self {
        std::sync::Arc::unwrap_or_clone(config.0)
    }
}

wire_enum!(TaskOutcome {
    Queued => "queued",
    Sent => "sent",
    Completed => "completed",
    AcceptedPendingApply => "accepted_pending_apply",
    Unknown => "unknown",
    Fault => "fault",
    Expired => "expired",
    Missing => "missing",
});

wire_enum!(Backend {
    Command => "command",
    Acs => "acs",
    Disabled => "disabled",
});

impl TaskOutcome {
    pub(super) fn from_task(task: &serde_json::Value) -> Self {
        Self::deserialize(&task["state"]).unwrap_or(Self::Missing)
    }
}

wire_enum!(ItemOutcome {
    Verified => "verified",
    Failed => "failed",
    Blocked => "blocked",
    Unknown => "unknown",
    AcceptedPendingApply => "accepted_pending_apply",
    AcceptedUnverified => "accepted_unverified",
    ReconcileRequired => "reconcile_required",
    Drift => "drift",
    Created => "created",
    Deleted => "deleted",
    Disabled => "disabled",
});

impl ItemOutcome {
    pub(super) fn from_result(result: &serde_json::Value) -> Option<Self> {
        Self::deserialize(&result["state"]).ok()
    }
}

wire_enum!(WanOwnershipState {
    Ready => "ready",
    Protected => "protected",
    AddingParent => "adding_parent",
    DiscoveringChild => "discovering_child",
    AddingChild => "adding_child",
    Deleting => "deleting",
});

wire_enum!(WifiPendingAction {
    Adding => "adding",
    Disabling => "disabling",
    Deleting => "deleting",
});

pub use fttr_api::{
    OperationPhase, SyncItemKind as ItemKind, WanStage as WanPhase, WifiStage as WifiPhase,
};

wire_enum!(ParameterKind {
    String => "string",
    Boolean => "boolean",
    UnsignedInt => "unsignedInt",
});

pub use crate::domain::WifiSecurity;

/// Values retain their CWMP lexical representation (including 0/1 booleans).
#[derive(Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub value: String,
    pub kind: ParameterKind,
    pub secret: bool,
}

#[derive(Clone, Serialize)]
pub struct Item {
    pub wlan_intent: Option<WlanIntent>,
    pub binding_intent: Option<BindingIntent>,
    pub id: String,
    pub kind: ItemKind,
    pub service_type: String,
    pub parameters: Vec<Parameter>,
    pub guard: Option<String>,
    pub blocked: Option<String>,
    pub wan_ref: Option<String>,
    pub auto_instance: Option<u32>,
    pub wan_intent: Option<WanIntent>,
}

pub use crate::domain::{
    BindingIntent, ConnectionMode, EndpointRef, VlanMode, WanIntent, WlanIntent,
};

#[derive(Clone)]
pub struct Device {
    pub id: String,
    pub optical_sn: String,
    pub cwmp_id: String,
    pub omci_adapter: String,
    pub bootstrap_adapter: String,
    pub provisioning_adapter: String,
    /// Runtime discovery result; never a configuration override.
    pub resolved_contract: Option<String>,
    pub connection_request: ConnectionRequest,
    pub items: Vec<Item>,
    pub wifi_cleanup: String,
    pub wifi_preserve: BTreeSet<u32>,
}

#[derive(Clone)]
pub struct ConnectionRequest {
    pub enabled: bool,
    pub defaults: bool,
    pub username: String,
    pub password: String,
    pub port: std::num::NonZeroU16,
    pub(crate) authorization: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for ConnectionRequest {
    fn default() -> Self {
        Self {
            enabled: true,
            defaults: true,
            username: String::new(),
            password: String::new(),
            port: const {
                match std::num::NonZeroU16::new(7547) {
                    Some(port) => port,
                    None => panic!("invalid CR port"),
                }
            },
            authorization: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl ConnectionRequest {
    pub fn credentials_available(&self) -> bool {
        !self.defaults
            && !self.username.is_empty()
            && !self.password.is_empty()
            && !self.username.contains(':')
            && [&self.username, &self.password]
                .iter()
                .all(|value| value.len() <= 1024 && !value.chars().any(char::is_control))
    }

    pub fn effective_freshness(&self, configured: u64) -> u64 {
        if self.enabled {
            configured.max(2 * 43200)
        } else {
            configured
        }
    }

    #[cfg(feature = "acs")]
    pub(crate) fn same_policy(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.defaults == other.defaults
            && self.username == other.username
            && self.password == other.password
            && self.port == other.port
            && std::sync::Arc::ptr_eq(&self.authorization, &other.authorization)
    }
}

impl Device {
    pub fn backend_kind(&self) -> Backend {
        match self.provisioning_adapter.as_str() {
            "none" => Backend::Disabled,
            "hl202-mqtt" => Backend::Command,
            _ => Backend::Acs, // ACS identifiers are validated by the configuration compiler.
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend_kind().as_str()
    }

    pub fn cwmp_enabled(&self) -> bool {
        self.bootstrap_adapter != "none" || self.backend() == "acs"
    }

    pub fn backend_view(&self) -> serde_json::Value {
        serde_json::json!({"configured":self.provisioning_adapter,
            "effective":self.backend(), "available":[self.backend()], "reason":""})
    }

    pub fn contract(&self) -> &'static crate::cwmp::contract::Contract {
        if let Some(id) = &self.resolved_contract {
            return crate::cwmp::registry::lookup(id).expect("discovered contract registered");
        }
        crate::cwmp::registry::lookup(if self.provisioning_adapter == "hl202-mqtt" {
            "hl202-cu"
        } else {
            &self.provisioning_adapter
        })
        .expect("validated provisioning contract")
    }

    pub fn omci_management(&self) -> bool {
        self.bootstrap_adapter == "omci-acs"
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcsRedirectScope {
    Lan,
    Management,
    Both,
}

impl AcsRedirectScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Management => "management",
            Self::Both => "both",
        }
    }

    pub fn includes_lan(self) -> bool {
        matches!(self, Self::Lan | Self::Both)
    }

    pub fn includes_management(self) -> bool {
        matches!(self, Self::Management | Self::Both)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcsPathMatch {
    Wildcard,
    Exact(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcsRedirectRuleConfig {
    pub id: String,
    pub name: Option<String>,
    pub enabled: bool,
    pub scope: AcsRedirectScope,
    pub target_ip: std::net::Ipv4Addr,
    pub target_prefix: u8,
    pub target_port: u16,
    pub path: AcsPathMatch,
    pub device: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcsBridgeConfig {
    pub enabled: bool,
    pub management_gateway: bool,
    pub rules: Vec<AcsRedirectRuleConfig>,
}

pub(crate) fn resolve_configured_device<'a>(
    devices: &'a [Device],
    target: &str,
) -> Result<&'a Device, String> {
    let matches: Vec<&Device> = devices
        .iter()
        .filter(|d| d.id == target || d.optical_sn.eq_ignore_ascii_case(target))
        .collect();
    match matches.as_slice() {
        [] => Err(format!("unknown device {target}")),
        [dev] => Ok(*dev),
        _ => Err(format!("ambiguous device target {target}")),
    }
}

#[derive(Clone, Default)]
pub struct Config {
    pub omci_auth: crate::omci_auth::Config,
    pub onu_pins: BTreeMap<String, u8>,
    /// Desired AN4475 slot count, absent on configurations that use the default.
    pub fiberhome_onu_count: Option<u8>,
    pub revision: String,
    pub business_revision: Option<String>,
    /// Fingerprints of compiled, immutable per-device business intent.
    pub device_revisions: BTreeMap<String, String>,
    pub enabled: bool,
    pub automatic: bool,
    pub triggers: BTreeSet<String>,
    pub interval_seconds: u64,
    pub freshness: u64,
    pub devices: Vec<Device>,
    pub assignments: BTreeMap<String, (String, String)>,
    pub groups: BTreeMap<String, String>,
    pub management: Option<Management>,
    pub management_network: Option<super::management_network::Desired>,
    pub mqtt_bridge_enabled: bool,
    pub acs_bridge: AcsBridgeConfig,
    pub allow_unsupported_as_bridge: bool,
    pub labels: BTreeMap<String, String>,
}

pub use fttr_api::ManagementDesired as Management;
impl Config {
    pub fn device_revision(&self, id: &str) -> &str {
        self.device_revisions
            .get(id)
            .map_or_else(|| self.business_revision(), String::as_str)
    }

    pub fn business_revision(&self) -> &str {
        self.business_revision.as_deref().unwrap_or(&self.revision)
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;

    #[test]
    fn scheduler_configuration_is_shared_until_a_reload_or_identity_mutation() {
        let original: SharedConfig = Config {
            revision: "before".into(),
            labels: BTreeMap::from([("device".into(), "unchanged".into())]),
            ..Default::default()
        }
        .into();
        let observer = original.clone();
        let mut next = original.clone();
        assert!(std::sync::Arc::ptr_eq(&observer.0, &next.0));
        assert_eq!(
            observer.labels["device"].as_ptr(),
            next.labels["device"].as_ptr()
        );
        next.revision = "after".into();
        next.labels.insert("device".into(), "changed".into());
        assert!(!std::sync::Arc::ptr_eq(&observer.0, &next.0));
        assert_eq!(observer.revision, "before");
        assert_eq!(observer.labels["device"], "unchanged");
        let owned: Config = next.into();
        assert_eq!(owned.revision, "after");
        assert_eq!(original.revision, "before");
    }

    #[test]
    fn optical_adapter_does_not_select_provisioning_transport() {
        let mut device = Device {
            id: "child".into(),
            optical_sn: "H3CT00000001".into(),
            cwmp_id: "pending:H3CT00000001".into(),
            omci_adapter: "hl202".into(),
            bootstrap_adapter: "none".into(),
            provisioning_adapter: "hl202-mqtt".into(),
            resolved_contract: None,
            connection_request: ConnectionRequest::default(),
            items: vec![],
            wifi_cleanup: "preserve".into(),
            wifi_preserve: BTreeSet::new(),
        };
        assert_eq!(device.backend(), "command");
        device.omci_adapter = "generic".into();
        assert_eq!(device.backend(), "command");
        device.provisioning_adapter = "hl202-cu".into();
        assert_eq!(device.backend(), "acs");
        device.omci_adapter = "hwtc".into();
        assert_eq!(device.backend(), "acs");
    }
}
