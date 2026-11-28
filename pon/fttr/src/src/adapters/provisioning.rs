//! C: transport plus parameter contract. No implicit transport or vendor fallback.
use crate::cwmp::{contract::Contract, registry};
use fttr_api::AdapterCapability;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Disabled,
    Mqtt,
    Acs,
}

pub struct Adapter {
    pub id: &'static str,
    pub transport: Transport,
    pub contract: Option<&'static Contract>,
}

pub fn available() -> Vec<Adapter> {
    let mut entries = vec![Adapter {
        id: "none",
        transport: Transport::Disabled,
        contract: None,
    }];
    if cfg!(all(feature = "device-hl202", feature = "mqtt")) {
        entries.push(Adapter {
            id: "hl202-mqtt",
            transport: Transport::Mqtt,
            contract: registry::lookup("hl202-cu").ok(),
        });
    }
    if cfg!(feature = "acs") {
        entries.extend(registry::available().into_iter().map(|contract| Adapter {
            id: contract.id,
            transport: Transport::Acs,
            contract: Some(contract),
        }));
    }
    entries
}

pub fn capabilities() -> Vec<AdapterCapability> {
    available()
        .into_iter()
        .map(|entry| AdapterCapability {
            id: entry.id,
            name: match entry.transport {
                Transport::Disabled => "No business provisioning",
                Transport::Mqtt => "HL202 MQTT direct",
                Transport::Acs => entry.id,
            },
        })
        .collect()
}

pub fn lookup(id: &str) -> Result<Adapter, String> {
    available()
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("provisioning adapter unavailable: {id}"))
}
