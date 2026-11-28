//! Resolve learned source MACs through bridge ports to currently online ONU identities.
//! Read-only sysfs observations; no bridge utility or forwarding changes are required.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};

const MAX_FDB_BYTES: u64 = 1024 * 1024;
const MAX_AGE_TICKS: u32 = 60 * 100; // Linux bridge sysfs exposes USER_HZ (100) ticks.

pub(crate) fn discover(
    onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
) -> Result<BTreeMap<String, String>, String> {
    read_links(onus, interfaces).map_err(|e| format!("ONU source-link discovery: {e}"))
}

fn read_links(
    onus: &BTreeMap<u16, String>,
    interfaces: &BTreeMap<u16, String>,
) -> io::Result<BTreeMap<String, String>> {
    let mut bridges: BTreeMap<String, BTreeMap<u16, String>> = BTreeMap::new();
    for (onu, sn) in onus {
        let Some(interface) = interfaces.get(onu) else {
            continue;
        };
        if interface.is_empty()
            || !interface
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid ONU interface",
            ));
        }
        let base = format!("/sys/class/net/{interface}");
        let port = match std::fs::read_to_string(format!("{base}/brport/port_no")) {
            Ok(value) => value,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let port = u16::from_str_radix(port.trim().trim_start_matches("0x"), 16).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid bridge port number")
        })?;
        let bridge = std::fs::read_link(format!("{base}/master"))?;
        let bridge = bridge
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid bridge name"))?;
        let ports = bridges.entry(bridge.into()).or_default();
        if ports.insert(port, sn.clone()).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate ONU bridge port",
            ));
        }
    }
    let mut result = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for (bridge, ports) in bridges {
        let mut data = Vec::new();
        std::fs::File::open(format!("/sys/class/net/{bridge}/brforward"))?
            .take(MAX_FDB_BYTES + 1)
            .read_to_end(&mut data)?;
        if data.len() as u64 > MAX_FDB_BYTES || data.len() % 16 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or oversized bridge FDB",
            ));
        }
        for (mac, sn) in resolve_fdb(&data, &ports) {
            if result.get(&mac).is_some_and(|old| old != &sn) {
                conflicts.insert(mac.clone());
            }
            result.insert(mac, sn);
        }
    }
    result.retain(|mac, _| !conflicts.contains(mac));
    Ok(result)
}

fn resolve_fdb(data: &[u8], ports: &BTreeMap<u16, String>) -> BTreeMap<String, String> {
    let mut learned: BTreeMap<String, BTreeSet<u16>> = BTreeMap::new();
    for entry in data.chunks_exact(16) {
        let age = u32::from_ne_bytes(entry[8..12].try_into().unwrap());
        if entry[7] != 0 || entry[0] & 1 != 0 || age > MAX_AGE_TICKS {
            continue; // Local, multicast and stale entries cannot identify MQTT senders.
        }
        let mac = entry[..6]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>();
        let port = u16::from(entry[6]) | (u16::from(entry[12]) << 8);
        learned.entry(mac).or_default().insert(port);
    }
    learned
        .into_iter()
        .filter_map(|(mac, seen)| {
            if seen.len() != 1 {
                return None;
            }
            ports
                .get(seen.first().unwrap())
                .cloned()
                .map(|sn| (mac, sn))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(port: u16, local: bool, age: u32) -> [u8; 16] {
        let mut e = [0; 16];
        e[..6].copy_from_slice(&[2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        e[6] = port as u8;
        e[7] = u8::from(local);
        e[8..12].copy_from_slice(&age.to_ne_bytes());
        e[12] = (port >> 8) as u8;
        e
    }

    #[test]
    fn resolves_dynamic_mac_to_exact_online_onu_including_high_port_byte() {
        let ports = BTreeMap::from([(275, "H3CT12345678".into())]);
        assert_eq!(
            resolve_fdb(&entry(275, false, 100), &ports)["02AABBCCDDEE"],
            "H3CT12345678"
        );
        for e in [
            entry(275, true, 0),
            entry(275, false, MAX_AGE_TICKS + 1),
            entry(18, false, 0),
        ] {
            assert!(resolve_fdb(&e, &ports).is_empty());
        }
    }

    #[test]
    fn conflicting_port_including_non_onu_port_is_not_an_identity() {
        let ports = BTreeMap::from([(19, "H3CT12345678".into())]);
        let data = [entry(19, false, 0), entry(8, false, 0)].concat();
        assert!(resolve_fdb(&data, &ports).is_empty());
    }
}
