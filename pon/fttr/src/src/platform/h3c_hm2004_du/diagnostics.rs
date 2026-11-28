//! Read-only H3C miniOLT diagnostics recovered from the pinned stock moltdbg.
//! This is an observed register layout, not a hardware revision detector.
pub use fttr_api::{
    FpgaDelta as Delta, FpgaDiagnosticRequest as Request, FpgaGrant as Grant, FpgaOnu as Onu,
    FpgaOnuDelta as OnuDelta, FpgaSnapshot as Snapshot,
};
use std::{
    collections::BTreeMap,
    io,
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const SOURCE_SHA256: &str = "b8036696b34d35f9aee68285f4b7f7b55d7f332a2e8e0b22059637db659ab2da";
pub const LIMITATIONS: &str = "Non-atomic register reads. Modulo deltas cannot distinguish resets or multiple wraps. Known in-daemon hardware writes invalidate continuity; external writers and unannounced hardware resets are not detected. Clear-on-read and saturation behavior are unverified. Global counters cannot be attributed to an ONU. Slots are not permanent identities. Grant bandwidth units are unknown. Labels follow stock moltdbg, not complete hardware semantics.";

// Labels and addresses from moltdbg 0x401a7c, 0x401ce0, 0x401e18,
// 0x401f04, 0x402030 and 0x402214. No reset or indirect-table operations.
const COUNTERS: &[(&str, u32)] = &[
    ("total_bursts", 0x010100a0),
    ("omci_invalid_alloc", 0x02010000),
    ("omci_too_long", 0x02010004),
    ("omci_afull", 0x02010008),
    ("omci_length", 0x02010028),
    ("ethernet_invalid_alloc", 0x0201000c),
    ("ethernet_assembled_too_long", 0x02010010),
    ("ethernet_assembled_afull", 0x02010014),
    ("ethernet_crc", 0x02010018),
    ("ethernet_short", 0x0201001c),
    ("ethernet_long", 0x02010020),
    ("ethernet_fifo_full", 0x02010024),
    ("upstream_info_fifo_full", 0x01010094),
    ("upstream_info_error", 0x01010098),
    ("upstream_drift_timeout", 0x0101009c),
    ("upstream_bch", 0x010100b0),
    ("upstream_xgem_length", 0x010100b4),
    ("upstream_xgem_fifo_full", 0x010100b8),
    ("upstream_no_sop", 0x010100bc),
    ("upstream_burst_compensate", 0x010100a4),
    ("downstream_rx_error", 0x03010010),
    ("downstream_ethernet_length", 0x03010014),
    ("downstream_omci_length", 0x03010018),
    ("downstream_ethernet_type", 0x0301001c),
    ("downstream_tx_fifo_full", 0x03010020),
];

fn grant(bandwidth_raw: u32, control_raw: u32) -> Grant {
    // moltdbg 0x401490 reports only these fields; retain the entire words.
    Grant {
        bandwidth_raw,
        control_raw,
        enabled_bit28: control_raw & (1 << 28) != 0,
        bandwidth: bandwidth_raw & 0xfffff,
        weight: ((bandwidth_raw >> 24) & 0x7f) as u8,
    }
}

const LABELS: &[(&str, &str)] = &[
    ("burst", "Total bursts"),
    ("omci", "OMCI alloc vld drop"),
    ("omci", "OMCI rx long drop"),
    ("omci", "OMCI packet afull drop"),
    ("omci", "OMCI packet length error"),
    ("ethernet", "ETH assemble invalid alloc id"),
    ("ethernet", "ETH assemble packet too long"),
    ("ethernet", "ETH assemble afull"),
    ("ethernet", "ETH CRC error"),
    ("ethernet", "ETH short <64B"),
    ("ethernet", "ETH long >2000B"),
    ("ethernet", "ETH fifo full"),
    ("upstream", "US info fifo full drop"),
    ("upstream", "US info error"),
    ("upstream", "US drift timeout"),
    ("upstream", "US BCH error"),
    ("upstream", "US XGEM length error"),
    ("upstream", "US XGEM fifo full / packet >2044B"),
    ("upstream", "US no SOP"),
    ("upstream", "US burst compensate"),
    ("downstream", "DS Rx error"),
    ("downstream", "DS ETH >2020B or <60B"),
    ("downstream", "DS OMCI !=64B"),
    ("downstream", "DS ETH type !=0x8100"),
    ("downstream", "DS TX fifo afull drop"),
];

// A nonblocking sequence marker: writers never wait for diagnostic reads.
// Separate active count handles overlapping writers (an odd/even epoch cannot).
#[derive(Default)]
struct Continuity {
    active: AtomicU64,
    epoch: AtomicU64,
}
impl Continuity {
    fn read(&self) -> io::Result<u64> {
        let before = self.epoch.load(Ordering::SeqCst);
        let active = self.active.load(Ordering::SeqCst);
        let after = self.epoch.load(Ordering::SeqCst);
        if active != 0 || before != after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "FPGA hardware context changing",
            ));
        }
        Ok(after)
    }
    fn write(&self) -> WriteMarker<'_> {
        self.active.fetch_add(1, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        WriteMarker(self)
    }
}
pub(super) struct WriteMarker<'a>(&'a Continuity);
impl Drop for WriteMarker<'_> {
    fn drop(&mut self) {
        self.0.epoch.fetch_add(1, Ordering::SeqCst);
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
static CONTINUITY: Continuity = Continuity {
    active: AtomicU64::new(0),
    epoch: AtomicU64::new(0),
};
pub(super) fn hardware_epoch() -> io::Result<u64> {
    CONTINUITY.read()
}
pub(super) fn hardware_write(address: u32) -> Option<WriteMarker<'static>> {
    (matches!(address, 0x20 | 0x50 | 0x84 | 0x01000004)
        || (0x01010000..=0x0101001c).contains(&address)
        || (0x01010070..=0x0101008c).contains(&address)
        || COUNTERS.iter().any(|(_, addr)| *addr == address))
    .then(|| CONTINUITY.write())
}

pub fn snapshot(
    request: &Request,
    mut read: impl FnMut(u32) -> io::Result<u32>,
) -> io::Result<Snapshot> {
    let slots = request.slots()?; // Validate before any hardware access.
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let started = Instant::now();
    let mut raw = BTreeMap::new();
    let mut get = |address| -> io::Result<u32> {
        if let Some(value) = raw.get(&address) {
            return Ok(*value);
        }
        let value = read(address).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("FPGA diagnostic read 0x{address:08x}: {error}"),
            )
        })?;
        raw.insert(address, value);
        Ok(value)
    };
    let main_bandwidth_raw = get(0x20)?;
    let mut onus = Vec::new();
    for slot in slots {
        let shift = 16 * (slot % 2);
        let packed = 4 * u32::from(slot / 2);
        let base = 0x05000098 + 16 * u32::from(slot);
        onus.push(Onu {
            slot,
            generation: None,
            lob: (get(0x01010000 + packed)? >> shift) as u16,
            bip: (get(0x01010070 + packed)? >> shift) as u16,
            management: grant(get(base)?, get(base + 4)?),
            business: grant(get(base + 8)?, get(base + 12)?),
        });
    }
    let counters = COUNTERS
        .iter()
        .map(|&(name, address)| Ok((name.to_string(), get(address)?)))
        .collect::<io::Result<_>>()?;
    Ok(Snapshot {
        schema_version: 2,
        source_sha256: SOURCE_SHA256.into(),
        limitations: LIMITATIONS.into(),
        started_unix_ms,
        read_duration_us: started.elapsed().as_micros() as u64,
        started_monotonic_ms: None,
        daemon_instance: None,
        hardware_epoch: None,
        main_bandwidth_raw,
        main_bandwidth_enabled: main_bandwidth_raw == 0x17258,
        onus,
        counters,
        counter_descriptors: COUNTERS
            .iter()
            .zip(LABELS)
            .map(
                |(&(key, address), &(group, label))| fttr_api::FpgaCounterDescriptor {
                    key: key.into(),
                    label: label.into(),
                    group: group.into(),
                    address: format!("0x{address:08x}"),
                    width_bits: 32,
                    scope: "global".into(),
                },
            )
            .collect(),
        raw_registers: raw
            .into_iter()
            .map(|(address, value)| (format!("0x{address:08x}"), value))
            .collect(),
    })
}

pub fn delta(first: &Snapshot, second: &Snapshot) -> Delta {
    let same_daemon =
        first.daemon_instance.is_some() && first.daemon_instance == second.daemon_instance;
    let clock = first
        .started_monotonic_ms
        .zip(second.started_monotonic_ms)
        .filter(|(a, b)| b > a);
    let continuous = same_daemon
        && first.schema_version == second.schema_version
        && first.source_sha256 == second.source_sha256
        && first.hardware_epoch.is_some()
        && first.hardware_epoch == second.hardware_epoch
        && clock.is_some();
    let onus = second
        .onus
        .iter()
        .map(|onu| {
            let previous = first.onus.iter().find(|old| old.slot == onu.slot);
            let verified = continuous
                && onu.generation.is_some()
                && previous.is_some_and(|old| old.generation == onu.generation);
            OnuDelta {
                slot: onu.slot,
                registration_continuity_verified: verified,
                lob_modulo_65536: previous
                    .filter(|_| verified)
                    .map(|old| onu.lob.wrapping_sub(old.lob)),
                bip_modulo_65536: previous
                    .filter(|_| verified)
                    .map(|old| onu.bip.wrapping_sub(old.bip)),
            }
        })
        .collect();
    Delta {
        elapsed_ms: clock.map_or(0, |(a, b)| b - a),
        same_daemon,
        hardware_continuity_verified: continuous,
        onus,
        counters_modulo_4294967296: continuous.then(|| {
            second
                .counters
                .iter()
                .filter_map(|(name, value)| {
                    first
                        .counters
                        .get(name)
                        .map(|old| (name.clone(), value.wrapping_sub(*old)))
                })
                .collect()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stock_layout_packed_slots_and_unknown_control_bits_are_preserved() {
        let mut addresses = std::collections::BTreeSet::new();
        let snap = snapshot(&Request::default(), |address| {
            assert!(addresses.insert(address), "packed words must be read once");
            Ok(match address {
                0x20 => 0x17258,
                0x01010000 => 0xabcd1234,
                0x01010070 => 0x4321fedc,
                0x0101001c => 0xbeef8765, // slots 14 / 15
                0x05000188 => 0x84004bc5, // slot 15 management
                0x0500018c => 0x11000514,
                _ => 0,
            })
        })
        .unwrap();
        assert!(snap.main_bandwidth_enabled);
        assert_eq!((snap.onus[0].lob, snap.onus[1].lob), (0x1234, 0xabcd));
        assert_eq!((snap.onus[0].bip, snap.onus[1].bip), (0xfedc, 0x4321));
        assert_eq!((snap.onus[14].lob, snap.onus[15].lob), (0x8765, 0xbeef));
        assert_eq!(
            snap.onus[15].management,
            Grant {
                bandwidth_raw: 0x84004bc5,
                control_raw: 0x11000514,
                enabled_bit28: true,
                bandwidth: 19397,
                weight: 4
            }
        );
    }
    #[test]
    fn invalid_requests_and_failed_reads_do_not_yield_partial_snapshots() {
        assert!(snapshot(&Request { onu_id: Some(16) }, |_| panic!(
            "unexpected access"
        ))
        .is_err());
        let error =
            snapshot(&Request::default(), |_| Err(io::Error::other("fixture"))).unwrap_err();
        assert!(error.to_string().contains("0x00000020"));
        assert!(serde_json::from_str::<Request>(r#"{"write":1}"#).is_err());
    }
    #[test]
    fn deltas_wrap_but_are_withheld_after_registration_or_daemon_changes() {
        let mut first = snapshot(&Request { onu_id: Some(0) }, |_| Ok(0)).unwrap();
        first.daemon_instance = Some("fixture".into());
        first.hardware_epoch = Some("0".into());
        first.started_monotonic_ms = Some(1);
        first.onus[0].generation = Some("7".into());
        first.onus[0].lob = 65530;
        first.counters.insert("upstream_bch".into(), u32::MAX - 1);
        let mut second: Snapshot =
            serde_json::from_value(serde_json::to_value(&first).unwrap()).unwrap();
        second.onus[0].lob = 10;
        second.started_monotonic_ms = Some(1001);
        second.counters.insert("upstream_bch".into(), 3);
        let change = delta(&first, &second);
        assert_eq!(change.onus[0].lob_modulo_65536, Some(16));
        assert_eq!(
            change.counters_modulo_4294967296.unwrap()["upstream_bch"],
            5
        );
        second.onus[0].generation = Some("8".into());
        assert!(delta(&first, &second).onus[0].lob_modulo_65536.is_none());
        second.hardware_epoch = Some("2".into());
        assert!(delta(&first, &second).counters_modulo_4294967296.is_none());
        second.daemon_instance = Some("restarted".into());
        assert!(delta(&first, &second).counters_modulo_4294967296.is_none());
    }
    #[test]
    fn overlapping_or_failed_writes_revoke_continuity_without_blocking_writers() {
        let state = Continuity::default();
        let before = state.read().unwrap();
        let a = state.write();
        let b = state.write();
        assert!(state.read().is_err());
        drop(a);
        assert!(state.read().is_err());
        drop(b);
        assert_ne!(state.read().unwrap(), before);
        assert_eq!(COUNTERS.len(), LABELS.len());
    }
}
