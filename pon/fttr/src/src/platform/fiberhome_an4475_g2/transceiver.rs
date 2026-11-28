//! UX3326S1 downstream monitor, following the stock 0x27 branch.
use super::{
    optical::{i2c_dev::I2cDevice, OpticalBus, Transfer},
    profile::UX3326S_DEVICE_ADDRESS,
};
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const FIELDS: [(&str, &str, u8); 4] = [
    ("temperature_celsius", "C", 0x6d),
    ("vcc_voltage", "V", 0x6f),
    ("laser_bias_current_ma", "mA", 0x71),
    ("tx_power_dbm", "dBm", 0x73),
];
const MAX_AGE: Duration = Duration::from_secs(15);

#[derive(Clone)]
struct Sample {
    at: Instant,
    values: [Result<u16, String>; 4],
}

fn cache() -> &'static Mutex<Option<Sample>> {
    static CACHE: OnceLock<Mutex<Option<Sample>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

// All four registers are < 0x80, so the module page select that calibration
// leaves on 0x85 should not affect them; this follows from the stock write
// plan targeting only 0x80..0xFF and is not independently verified. Compare
// readings before/after a calibration run during the next bench window.
fn read_words<T: Transfer>(bus: &mut OpticalBus<T>) -> [Result<u16, String>; 4] {
    FIELDS.map(|(_, _, register)| {
        let mut bytes = [0; 2];
        bus.read(UX3326S_DEVICE_ADDRESS, register, &mut bytes)
            .map(|()| u16::from_be_bytes(bytes))
            .map_err(|error| error.to_string())
    })
}

/// Called only by the optical owner, never concurrently with calibration:
/// both run on the fsl-loop thread, and the offline fttrctl path is excluded
/// by the owner flock. A poisoned cache mutex silently keeps the stale sample.
pub(super) fn refresh() {
    let values = match I2cDevice::open(Path::new("/dev/i2c-0")) {
        Ok(device) => read_words(&mut OpticalBus::new(device)),
        Err(error) => std::array::from_fn(|_| Err(error.to_string())),
    };
    if let Ok(mut slot) = cache().lock() {
        *slot = Some(Sample {
            at: Instant::now(),
            values,
        });
    }
}

/// SFF-8472 conventional scales; the register addresses and scaling follow the
/// stock convention but are not yet cross-checked against measured hardware.
/// `raw == 0` on the power register is the stock -40 dBm sentinel, not a
/// measurement, and the evidence text keeps the raw value for that reason.
fn convert(index: usize, raw: u16) -> f32 {
    match index {
        0 => f32::from(raw as i16) / 256.0,
        1 => f32::from(raw) / 10000.0,
        2 => f32::from(raw) / 500.0,
        3 if raw == 0 => -40.0,
        3 => 10.0 * (f32::from(raw) / 10000.0).log10(),
        _ => unreachable!(),
    }
}

pub(super) fn snapshot() -> ([Option<f32>; 4], fttr_api::Measurements) {
    let sample = cache().lock().ok().and_then(|slot| slot.clone());
    let mut values = [None; 4];
    let mut evidence = fttr_api::Measurements::new();
    for (index, (name, unit, register)) in FIELDS.into_iter().enumerate() {
        let source = format!("UX3326S1/I2C 0x63 register {register:#04x}");
        let measurement = match &sample {
            Some(sample) if sample.at.elapsed() <= MAX_AGE => match &sample.values[index] {
                Ok(raw) => {
                    values[index] = Some(convert(index, *raw));
                    let mut evidence = fttr_api::MeasurementEvidence::observed(
                        source,
                        unit,
                        format!("{raw:#06x}"),
                    );
                    evidence.age_seconds = Some(sample.at.elapsed().as_secs());
                    evidence
                }
                Err(error) => fttr_api::MeasurementEvidence::unavailable(
                    source,
                    unit,
                    format!("read_failed: {error}"),
                ),
            },
            Some(_) => fttr_api::MeasurementEvidence::unavailable(source, unit, "sample_expired"),
            None => fttr_api::MeasurementEvidence::unavailable(source, unit, "not_yet_sampled"),
        };
        evidence.insert(name.into(), measurement);
    }
    (values, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    struct Mock(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl Transfer for Mock {
        fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> io::Result<usize> {
            assert_eq!(address, 0x63);
            assert_eq!(read.len(), 2);
            self.0.borrow_mut().push(write[0]);
            read.copy_from_slice(&[0x12, 0x34]);
            Ok(2)
        }
    }
    #[test]
    fn stock_registers_are_read_as_four_independent_big_endian_words() {
        let registers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut bus = OpticalBus::new(Mock(registers.clone()));
        assert!(read_words(&mut bus).iter().all(|v| *v == Ok(0x1234)));
        assert_eq!(*registers.borrow(), [0x6d, 0x6f, 0x71, 0x73]);
    }
    #[test]
    fn stock_conversions_include_signed_temperature_and_zero_power() {
        assert_eq!(convert(0, 0xff00), -1.0);
        assert_eq!(convert(1, 32113), 3.2113);
        assert_eq!(convert(2, 11293), 22.586);
        assert_eq!(convert(3, 0), -40.0);
        assert!((convert(3, 20000) - 3.0103).abs() < 0.001);
    }
}
