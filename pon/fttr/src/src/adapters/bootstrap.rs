//! B: one selected bootstrap strategy may perform multiple verified steps.
use fttr_api::AdapterCapability;

pub fn available() -> Vec<AdapterCapability> {
    let mut entries = vec![AdapterCapability {
        id: "none",
        name: "No ACS bootstrap",
    }];
    if cfg!(feature = "acs") {
        entries.extend([
            AdapterCapability {
                id: "manual-acs",
                name: "Manual ACS settings",
            },
            AdapterCapability {
                id: "omci-acs",
                name: "Standard OMCI ACS association",
            },
        ]);
        if cfg!(all(feature = "device-hl202", feature = "mqtt")) {
            entries.push(AdapterCapability {
                id: "hl202-mqtt-acs",
                name: "HL202 MQTT management WAN + ACS",
            });
        }
    }
    entries
}

pub fn validate(id: &str) -> Result<(), String> {
    if !available().iter().any(|entry| entry.id == id) {
        return Err(format!("bootstrap adapter unavailable: {id}"));
    }
    Ok(())
}
