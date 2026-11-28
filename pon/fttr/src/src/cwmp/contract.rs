//! A contract describes paths and behavior, not a device's object instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Parent,
    Connection,
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub name: &'static str,
    pub location: Location,
    pub kind: ValueKind,
    pub readback: ReadbackRule,
    pub ignored_wlan_references: &'static [u32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    String,
    Boolean,
    UnsignedInt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadbackRule {
    Exact,
    Boolean,
    Set,
    ServiceSet,
    Secret,
}
#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unverified,
    Unsupported,
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Capabilities {
    pub wan: Support,
    pub wlan: Support,
    pub binding: Support,
}
pub const UNVERIFIED: Capabilities = Capabilities {
    wan: Support::Unverified,
    wlan: Support::Unverified,
    binding: Support::Unverified,
};
#[derive(Clone, Copy, Debug)]
pub enum ReferenceEncoding {
    FullPathsComma,
    EndpointFlags {
        lan: &'static [Field],
        wlan: &'static [Field],
    },
}
#[derive(Clone, Copy, Debug)]
pub struct OperationRequirements {
    pub disable_wan_before_write: bool,
    pub settle_seconds: u64,
}
impl Field {
    pub fn path(self, parent: &str, connection: &str) -> String {
        format!(
            "{}{}",
            match self.location {
                Location::Parent => parent,
                Location::Connection => connection,
            },
            self.name
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WanContract {
    pub service: Field,
    pub vlan: Field,
    pub enabled: Option<Field>,
    pub priority: Field,
    pub binding: Option<Field>,
    pub cloud_cpe: Option<Field>,
}
impl WanContract {
    pub fn tagged(self, mut value: impl FnMut(&str) -> Option<String>) -> Option<bool> {
        if let Some(field) = self.enabled {
            match value(field.name)?.as_str() {
                "1" | "true" => Some(true),
                "0" | "false" => Some(false),
                _ => None,
            }
        } else {
            value(self.vlan.name)?.parse::<u32>().ok().map(|v| v != 0)
        }
    }
    /// Preserve current readback requirements: parent VLAN changes also need
    /// parent priority; connection-local priority is checked during configuration.
    pub fn classification_paths(self, parent: &str, connection: &str) -> Vec<String> {
        let mut paths = vec![self.service.path(parent, connection)];
        if let Some(field) = self.enabled {
            paths.push(field.path(parent, connection));
        }
        paths.push(self.vlan.path(parent, connection));
        if self.priority.location == Location::Parent {
            paths.push(self.priority.path(parent, connection));
        }
        paths
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WlanContract {
    pub radio_slots: Option<[[u32; 4]; 2]>,
    pub wpa2_beacon: &'static str,
    pub wpa_wpa2_beacon: &'static str,
    pub nat_writable_exception: bool,
    pub passphrase_fallback: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Contract {
    pub supported_services: &'static [&'static str],
    pub id: &'static str,
    pub vendor: &'static str,
    pub wan: WanContract,
    pub wlan: WlanContract,
    /// Only observed/validated implementations may authorize automatic writes.
    pub writes_verified: bool,
    pub capability_groups: Capabilities,
    pub references: Option<ReferenceEncoding>,
    pub requirements: OperationRequirements,
    pub service_encoding: Option<super::codec::ServiceEncoding>,
}

impl Contract {
    pub fn binding_fields(&self) -> Vec<Field> {
        if let Some(ReferenceEncoding::EndpointFlags { lan, wlan }) = self.references {
            lan.iter().chain(wlan).copied().collect()
        } else {
            self.wan.binding.into_iter().collect()
        }
    }

    pub fn capabilities(&self) -> serde_json::Value {
        serde_json::json!({
            "wan":self.capability_groups.wan,
            "wlan":self.capability_groups.wlan,
            "binding":self.capability_groups.binding,
            "combined_services":if self.service_encoding.is_some() {"supported"} else {"unverified"}
        })
    }
}

pub const fn connection(name: &'static str) -> Field {
    Field {
        name,
        location: Location::Connection,
        kind: ValueKind::String,
        readback: ReadbackRule::Exact,
        ignored_wlan_references: &[],
    }
}
pub const fn parent(name: &'static str) -> Field {
    Field {
        name,
        location: Location::Parent,
        kind: ValueKind::String,
        readback: ReadbackRule::Exact,
        ignored_wlan_references: &[],
    }
}

pub const fn unsigned(mut field: Field) -> Field {
    field.kind = ValueKind::UnsignedInt;
    field
}
pub const fn boolean(mut field: Field) -> Field {
    field.kind = ValueKind::Boolean;
    field.readback = ReadbackRule::Boolean;
    field
}
pub const fn reference(mut field: Field) -> Field {
    field.readback = ReadbackRule::Set;
    field
}
pub const fn services(mut field: Field) -> Field {
    field.readback = ReadbackRule::ServiceSet;
    field
}
pub const fn ignore_wlan(mut field: Field, instances: &'static [u32]) -> Field {
    field.ignored_wlan_references = instances;
    field
}

pub fn equivalent_field(field: Field, desired: &str, observed: &str) -> bool {
    match field.readback {
        ReadbackRule::Boolean => matches!(
            (desired, observed),
            ("1" | "true", "1" | "true") | ("0" | "false", "0" | "false")
        ),
        ReadbackRule::ServiceSet => {
            super::codec::service_tokens(desired)
                .map(str::to_ascii_uppercase)
                .collect::<std::collections::BTreeSet<_>>()
                == super::codec::service_tokens(observed)
                    .map(str::to_ascii_uppercase)
                    .collect()
        }
        ReadbackRule::Set => {
            let tokens = |value: &str| {
                value
                    .split(',')
                    .filter(|v| !v.is_empty())
                    .filter(|v| {
                        !v.trim_end_matches('.')
                            .rsplit_once(".WLANConfiguration.")
                            .and_then(|(_, n)| n.parse::<u32>().ok())
                            .is_some_and(|n| field.ignored_wlan_references.contains(&n))
                    })
                    .map(str::to_owned)
                    .collect::<std::collections::BTreeSet<_>>()
            };
            tokens(desired) == tokens(observed)
        }
        ReadbackRule::Exact | ReadbackRule::Secret => desired == observed,
    }
}
