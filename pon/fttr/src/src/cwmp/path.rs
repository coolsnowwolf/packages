//! Actual object instances are supplied by inventory, never inferred from intent.
use crate::provisioning::model::EndpointRef;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct WanObjects {
    pub parent: u32,
    pub connection: u32,
    pub kind: ConnectionKind,
}
#[derive(Clone, Copy, Debug)]
pub enum ConnectionKind {
    Ip,
    Ppp,
}
impl WanObjects {
    pub fn paths(&self) -> Result<(String, String), String> {
        if self.parent == 0 || self.connection == 0 {
            return Err("unresolved WAN instance".into());
        }
        let parent = format!(
            "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.{}.",
            self.parent
        );
        let kind = match self.kind {
            ConnectionKind::Ip => "WANIPConnection",
            ConnectionKind::Ppp => "WANPPPConnection",
        };
        let connection = format!("{parent}{kind}.{}.", self.connection);
        Ok((parent, connection))
    }
}

#[derive(Clone, Default)]
pub struct EndpointMap(pub BTreeMap<EndpointRef, String>);

pub fn parse_endpoint(value: &str) -> Result<EndpointRef, String> {
    let parts: Vec<_> = value.split(':').collect();
    let positive = |v: &str| {
        v.parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| "invalid endpoint number".to_owned())
    };
    match parts.as_slice() {
        ["lan", n] => Ok(EndpointRef::Lan(positive(n)?)),
        ["ssid", radio, slot] => {
            let radio = positive(radio)?;
            let slot = positive(slot)?;
            if !matches!(radio, 2 | 5) || slot > 4 {
                return Err("unsupported radio/SSID slot".into());
            }
            Ok(EndpointRef::Ssid {
                radio: radio as u8,
                slot,
            })
        }
        _ => Err("endpoint must be lan:N or ssid:2|5:N".into()),
    }
}
impl EndpointMap {
    pub fn resolve(&self, endpoint: &EndpointRef) -> Result<&str, String> {
        let path = self
            .0
            .get(endpoint)
            .ok_or_else(|| format!("unresolved endpoint {endpoint:?}"))?;
        let tail = path
            .strip_prefix("InternetGatewayDevice.LANDevice.1.")
            .ok_or("endpoint is outside the supported LAN object")?;
        let (kind, instance) = tail
            .split_once('.')
            .ok_or("endpoint is not an instance path")?;
        if !matches!(kind, "LANEthernetInterfaceConfig" | "WLANConfiguration")
            || !instance.bytes().all(|b| b.is_ascii_digit())
            || instance.parse::<u32>().ok().is_none_or(|n| n == 0)
        {
            return Err("invalid endpoint instance path".into());
        }
        if matches!(endpoint, EndpointRef::Lan(_)) && kind != "LANEthernetInterfaceConfig"
            || matches!(endpoint, EndpointRef::Ssid { .. }) && kind != "WLANConfiguration"
        {
            return Err("endpoint kind conflicts with logical reference".into());
        }
        Ok(path)
    }
}
