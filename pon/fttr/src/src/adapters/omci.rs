//! A: optical bridge adapter selection, independent of management transports.
use fttr_api::AdapterCapability;

pub fn available() -> Vec<AdapterCapability> {
    let mut entries = vec![
        AdapterCapability {
            id: "auto",
            name: "Auto",
        },
        AdapterCapability {
            id: "generic",
            name: "Generic G.988 Bridge",
        },
    ];
    for (enabled, id, name) in [
        (cfg!(feature = "device-hl202"), "hl202", "H3C HL202"),
        (cfg!(feature = "device-hwtc"), "hwtc", "Huawei HWTC"),
        (cfg!(feature = "device-star"), "star", "STAR SG631"),
        (cfg!(feature = "device-zte"), "zte", "ZTE G1612 / F650A"),
        (cfg!(feature = "device-scty"), "scty", "SCTY TY-FS803G"),
        (cfg!(feature = "device-fiberhome"), "fiberhome", "FiberHome"),
        (
            cfg!(feature = "device-realtek"),
            "realtek",
            "Realtek TYFCA1",
        ),
    ] {
        if enabled {
            entries.push(AdapterCapability { id, name });
        }
    }
    entries
}

pub fn validate(id: &str) -> Result<(), String> {
    available()
        .iter()
        .any(|entry| entry.id == id)
        .then_some(())
        .ok_or_else(|| format!("OMCI adapter unavailable: {id}"))
}
