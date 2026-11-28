//! Business intent, independent of device adapters and wire parameter paths.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WlanIntent {
    pub endpoint: EndpointRef,
    pub ssid: String,
    pub enabled: Option<bool>,
    pub broadcast: Option<bool>,
    pub security: Option<WifiSecurity>,
    pub key: Option<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct BindingIntent {
    pub endpoints: Vec<EndpointRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WanIntent {
    pub services: Vec<String>,
    pub vlan: VlanMode,
    pub priority: u8,
    pub mode: ConnectionMode,
    pub bindings: Vec<EndpointRef>,
    pub internet: bool,
    pub enabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum VlanMode {
    Untagged,
    Tagged(u16),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ConnectionMode {
    Bridge,
    Routed,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum EndpointRef {
    Lan(u32),
    Ssid { radio: u8, slot: u32 },
    Named(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WifiSecurity {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "wpa2-psk")]
    Wpa2Psk,
    #[serde(rename = "wpa3-sae")]
    Wpa3Sae,
    #[serde(rename = "wpa2-psk/wpa3-sae")]
    Wpa2PskWpa3Sae,
    #[serde(rename = "wpa-psk/wpa2-psk")]
    WpaPskWpa2Psk,
}

impl WifiSecurity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Wpa2Psk => "wpa2-psk",
            Self::Wpa3Sae => "wpa3-sae",
            Self::Wpa2PskWpa3Sae => "wpa2-psk/wpa3-sae",
            Self::WpaPskWpa2Psk => "wpa-psk/wpa2-psk",
        }
    }
}

impl std::fmt::Display for WifiSecurity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
