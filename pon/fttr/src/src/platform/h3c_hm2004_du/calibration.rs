//! HM2004-DU UX3326 calibration snapshots. See EVIDENCE/HM2004-DU-STOCK-TX-CONTROL.md.
use super::fmcs::{self as bosa, BosaCalibration, DEFAULT_UX3326_CALIBRATION_DATA};
use fttr_api::{
    CalibrationMode, CalibrationSource, CalibrationState, CalibrationVersion,
    TemperatureCompensationStatus,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    sync::{Arc, RwLock},
};

pub const FIRMWARE_PATH: &str = "/lib/firmware/modified_UX3326.bin";
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub bytes: [u8; 288],
    pub telemetry: BosaCalibration,
    pub table: [u16; 84],
    pub version: CalibrationVersion,
}

impl Snapshot {
    pub fn parse(bytes: &[u8], source: CalibrationSource) -> io::Result<Self> {
        if !matches!(bytes.len(), 56 | 288)
            || bytes.iter().all(|&v| v == 0)
            || bytes.iter().all(|&v| v == 255)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "calibration must contain non-blank 56 or 288 byte data",
            ));
        }
        let supplemented = bytes.len() == 56;
        let mut effective = DEFAULT_UX3326_CALIBRATION_DATA;
        effective[..bytes.len()].copy_from_slice(bytes);
        // A register-only file cannot authorize a historical temperature table.
        if supplemented {
            effective[0x60] = 0;
        }
        if effective[0x60] > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Mod_Lut_EN must be 0 or 1",
            ));
        }
        let telemetry = BosaCalibration::from_bytes(&effective).unwrap();
        let table = std::array::from_fn(|i| {
            u16::from_be_bytes([effective[0x70 + i * 2], effective[0x71 + i * 2]])
        });
        if effective[0x60] == 1
            && (telemetry.temp_primary.1 == 0 || table.iter().any(|&v| v > 1023))
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "enabled temperature compensation requires a nonzero denominator and 10-bit table values"));
        }
        Ok(Self {
            bytes: effective,
            telemetry,
            table,
            version: CalibrationVersion {
                source,
                raw_sha256: hash(bytes),
                effective_sha256: hash(&effective),
                size: bytes.len() as u32,
                supplemented,
                temperature_compensation_enabled: effective[0x60] == 1,
            },
        })
    }

    pub fn target(&self, raw: u16) -> io::Result<Option<(u8, u16)>> {
        let (n, d, o) = self.telemetry.temp_primary;
        let product = i32::try_from(u32::from(raw) * u32::from(n)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "temperature multiplication exceeds signed 32-bit range",
            )
        })?;
        if d == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temperature denominator is zero",
            ));
        }
        // Signed BE16, not the stock daemon's unconditional negative sign extension.
        let q = (product / i32::from(d))
            .checked_add(i32::from(o))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "temperature offset exceeds signed 32-bit range",
                )
            })?;
        let index = ((q >> 8) + 40) >> 1;
        Ok((0..84)
            .contains(&index)
            .then(|| (index as u8, self.table[index as usize])))
    }
}

pub fn firmware() -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    match fs::File::open(FIRMWARE_PATH) {
        Ok(file) => {
            file.take(289).read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    Ok(bytes)
}

pub fn select(
    mode: CalibrationMode,
    factory: Option<&[u8]>,
    firmware: &[u8],
) -> io::Result<Snapshot> {
    let parse = |source| match source {
        CalibrationSource::Factory => factory
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Factory calibration not found"))
            .and_then(|b| Snapshot::parse(b, source)),
        CalibrationSource::Firmware => Snapshot::parse(firmware, source),
        CalibrationSource::Builtin => Snapshot::parse(&DEFAULT_UX3326_CALIBRATION_DATA, source),
    };
    match mode {
        CalibrationMode::Factory => parse(CalibrationSource::Factory),
        CalibrationMode::Firmware => parse(CalibrationSource::Firmware),
        CalibrationMode::Builtin => parse(CalibrationSource::Builtin),
        CalibrationMode::Auto => parse(CalibrationSource::Factory)
            .or_else(|_| parse(CalibrationSource::Firmware))
            .or_else(|_| parse(CalibrationSource::Builtin)),
    }
}

pub fn resolve() -> io::Result<Snapshot> {
    let mode = crate::optical::load_calibration_mode()?;
    let factory = bosa::read_factory_calibration();
    // An unreadable unselected file must not prevent an explicit Factory selection.
    let firmware = match firmware() {
        Ok(b) => b,
        Err(e) if mode == CalibrationMode::Firmware => return Err(e),
        Err(_) => Vec::new(),
    };
    select(mode, factory.as_ref().map(|v| v.as_slice()), &firmware)
}

#[derive(Default)]
pub struct Runtime {
    pub active: Option<Arc<Snapshot>>,
    pub hardware_applied: bool,
    pub compensation: TemperatureCompensationStatus,
}
impl Runtime {
    pub fn transition(
        &mut self,
        candidate: Option<Snapshot>,
        hardware: impl FnOnce(Option<&Snapshot>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.hardware_applied = false;
        self.compensation.enabled = false;
        self.compensation.reason = if candidate.is_some() {
            "initializing"
        } else {
            "optical_disabled"
        }
        .into();
        bosa::invalidate_optical_cache();
        match hardware(candidate.as_ref()) {
            Ok(()) => {
                if let Some(candidate) = candidate {
                    self.compensation = TemperatureCompensationStatus {
                        enabled: candidate.version.temperature_compensation_enabled,
                        reason: if candidate.version.supplemented {
                            "partial_calibration"
                        } else if !candidate.version.temperature_compensation_enabled {
                            "table_disabled"
                        } else {
                            "awaiting_first_check"
                        }
                        .into(),
                        calibration_sha256: Some(candidate.version.effective_sha256.clone()),
                        ..Default::default()
                    };
                    self.active = Some(Arc::new(candidate));
                    self.hardware_applied = true;
                }
                Ok(())
            }
            Err(e) => {
                self.compensation.reason = "transition_failed".into();
                self.compensation.error = Some(e.to_string());
                Err(e)
            }
        }
    }
}
// Readers hold this across ADC reads; transitions hold it across reset/program/publish.
// Lock order: RUNTIME -> bosa::OPTICAL_CACHE. No I/O uses an independently reloaded blob.
pub static RUNTIME: RwLock<Runtime> = RwLock::new(Runtime {
    active: None,
    hardware_applied: false,
    compensation: TemperatureCompensationStatus {
        enabled: false,
        reason: String::new(),
        calibration_sha256: None,
        raw_temperature: None,
        table_index: None,
        target_code: None,
        last_check_unix: None,
        last_write_unix: None,
        error: None,
    },
});

pub fn state(mode: CalibrationMode, candidate: io::Result<&Snapshot>) -> CalibrationState {
    let runtime = RUNTIME.read().unwrap();
    let (candidate, candidate_error) = match candidate {
        Ok(s) => (Some(s.version.clone()), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let active = runtime.active.as_ref().map(|s| s.version.clone());
    let pending = !runtime.hardware_applied
        || match (&candidate, &active) {
            (Some(c), Some(a)) => c.source != a.source || c.effective_sha256 != a.effective_sha256,
            _ => true,
        };
    CalibrationState {
        selected: mode,
        candidate,
        candidate_error,
        active,
        hardware_applied: runtime.hardware_applied,
        pending,
        temperature_compensation: runtime.compensation.clone(),
    }
}

pub trait CompensationIo {
    fn enabled(&mut self) -> io::Result<bool>;
    fn read(&mut self, register: u32) -> io::Result<u8>;
    fn write(&mut self, register: u32, value: u8) -> io::Result<()>;
}
pub struct Hardware;
impl CompensationIo for Hardware {
    fn enabled(&mut self) -> io::Result<bool> {
        bosa::get_optical_tx_enable()
    }
    fn read(&mut self, register: u32) -> io::Result<u8> {
        let value = bosa::read_bosa_reg(register)? as u8;
        std::thread::sleep(std::time::Duration::from_millis(1));
        Ok(value)
    }
    fn write(&mut self, register: u32, value: u8) -> io::Result<()> {
        bosa::write_bosa_reg(register, value.into())?;
        std::thread::sleep(std::time::Duration::from_millis(1));
        Ok(())
    }
}

fn compensate(
    snapshot: &Snapshot,
    status: &mut TemperatureCompensationStatus,
    io: &mut impl CompensationIo,
) -> io::Result<bool> {
    status.error = None;
    status.raw_temperature = None;
    status.table_index = None;
    status.target_code = None;
    let raw = match io
        .read(0x2e)
        .and_then(|hi| io.read(0x2f).map(|lo| u16::from_be_bytes([hi, lo])))
    {
        Ok(raw) => raw,
        Err(e) => {
            status.reason = "adc_read_failed".into();
            status.error = Some(e.to_string());
            return Ok(false);
        }
    };
    status.raw_temperature = Some(raw);
    let (index, code) = match snapshot.target(raw) {
        Ok(Some(target)) => target,
        Ok(None) => {
            status.reason = "temperature_index_out_of_range".into();
            return Ok(false);
        }
        Err(e) => {
            status.reason = "invalid_temperature".into();
            status.error = Some(e.to_string());
            return Ok(false);
        }
    };
    status.table_index = Some(index);
    status.target_code = Some(code);
    let target = [(code >> 2) as u8, (code & 3) as u8];
    let current = match io.read(6).and_then(|hi| io.read(7).map(|lo| [hi, lo])) {
        Ok(pair) => pair,
        Err(e) => {
            status.reason = "code_read_failed".into();
            status.error = Some(e.to_string());
            return Ok(false);
        }
    };
    if current == target {
        status.reason = "unchanged".into();
        return Ok(false);
    }
    // No gate or 0x27 latch: mirrors the observed stock tlut write helper.
    io.write(6, target[0])?;
    io.write(7, target[1])?;
    // The code pair shares the I2C bus with telemetry reads, so an immediate readback
    // can observe a torn value and report a false failure. Settle briefly, then retry
    // the write once before treating the mismatch as real.
    std::thread::sleep(std::time::Duration::from_millis(2));
    if [io.read(6)?, io.read(7)?] != target {
        io.write(6, target[0])?;
        io.write(7, target[1])?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        if [io.read(6)?, io.read(7)?] != target {
            return Err(io::Error::other("temperature code pair readback mismatch"));
        }
    }
    status.reason = "updated".into();
    Ok(true)
}

#[derive(Default)]
pub struct Compensator {
    next: Option<std::time::Instant>,
}
impl Compensator {
    pub fn poll(
        &mut self,
        now: std::time::Instant,
        allowed: bool,
        io: &mut impl CompensationIo,
    ) -> io::Result<()> {
        let mut runtime = RUNTIME.write().unwrap();
        self.poll_runtime(&mut runtime, now, allowed, io)
    }

    fn poll_runtime(
        &mut self,
        runtime: &mut Runtime,
        now: std::time::Instant,
        allowed: bool,
        io: &mut impl CompensationIo,
    ) -> io::Result<()> {
        if !allowed || !runtime.hardware_applied {
            self.next = None;
            // An owner which has left Enabled cannot vouch for the live register
            // image (for example after an unexpected reset observed by Status).
            if !allowed && runtime.hardware_applied {
                runtime.hardware_applied = false;
                bosa::invalidate_optical_cache();
            }
            runtime.compensation.enabled = false;
            if runtime.compensation.error.is_none() {
                runtime.compensation.reason = "owner_not_enabled".into();
            }
            return Ok(());
        }
        let snapshot = runtime.active.as_ref().expect("applied calibration exists");
        if !snapshot.version.temperature_compensation_enabled {
            return Ok(());
        }
        let status = &mut runtime.compensation;
        let immediate = !status.enabled || status.reason == "awaiting_first_check";
        status.enabled = true;
        if !immediate && self.next.is_some_and(|next| now < next) {
            return Ok(());
        }
        self.next = Some(now + std::time::Duration::from_secs(60));
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u32::try_from(d.as_secs()).ok());
        status.last_check_unix = timestamp;
        let result = io.enabled().and_then(|enabled| {
            if !enabled {
                return Err(io::Error::other("BOSA reset line is not released"));
            }
            compensate(snapshot, status, io)
        });
        match result {
            Ok(wrote) => {
                if wrote {
                    status.last_write_unix = timestamp;
                    bosa::invalidate_optical_cache();
                }
                Ok(())
            }
            Err(e) => {
                runtime.hardware_applied = false;
                status.enabled = false;
                status.reason = "write_failed".into();
                status.error = Some(e.to_string());
                bosa::invalidate_optical_cache();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn stock() -> Snapshot {
        Snapshot::parse(&DEFAULT_UX3326_CALIBRATION_DATA, CalibrationSource::Builtin).unwrap()
    }
    #[derive(Default)]
    struct FakeIo {
        registers: std::collections::BTreeMap<u32, u8>,
        writes: Vec<(u32, u8)>,
        reads: usize,
        fail_read: Option<u32>,
        fail_write: Option<u32>,
        disabled: bool,
    }
    impl FakeIo {
        fn warm() -> Self {
            Self {
                registers: [(0x2e, 0x59), (0x2f, 0x80), (6, 0x3c), (7, 2)].into(),
                ..Default::default()
            }
        }
    }
    impl CompensationIo for FakeIo {
        fn enabled(&mut self) -> io::Result<bool> {
            Ok(!self.disabled)
        }
        fn read(&mut self, register: u32) -> io::Result<u8> {
            self.reads += 1;
            if self.fail_read == Some(register) {
                return Err(io::Error::other("read failure"));
            }
            Ok(*self.registers.get(&register).unwrap_or(&0))
        }
        fn write(&mut self, register: u32, value: u8) -> io::Result<()> {
            self.writes.push((register, value));
            if self.fail_write == Some(register) {
                return Err(io::Error::other("write failure"));
            }
            self.registers.insert(register, value);
            Ok(())
        }
    }
    #[test]
    fn versions_change_only_after_verified_apply_and_failed_apply_retains_history() {
        let a = stock();
        let mut b_bytes = a.bytes;
        b_bytes[6] += 1;
        b_bytes[0x53] += 10;
        let b = Snapshot::parse(&b_bytes, CalibrationSource::Firmware).unwrap();
        let mut runtime = Runtime::default();
        runtime.transition(Some(a.clone()), |_| Ok(())).unwrap();
        let old = bosa::calc_tx_power(0x2441, &runtime.active.as_ref().unwrap().telemetry);
        // Parsing/uploading another snapshot alone does not update live coefficients.
        assert_ne!(a.version.effective_sha256, b.version.effective_sha256);
        assert_eq!(runtime.active.as_ref().unwrap().bytes, a.bytes);
        runtime
            .transition(Some(b.clone()), |s| {
                assert_eq!(s.unwrap().bytes, b.bytes);
                Ok(())
            })
            .unwrap();
        assert_ne!(
            old,
            bosa::calc_tx_power(0x2441, &runtime.active.as_ref().unwrap().telemetry)
        );
        assert_eq!(
            runtime.compensation.calibration_sha256.as_ref(),
            Some(&b.version.effective_sha256)
        );
        assert!(runtime
            .transition(Some(a), |_| Err(io::Error::other("readback mismatch")))
            .is_err());
        assert_eq!(
            runtime.active.as_ref().unwrap().version.effective_sha256,
            b.version.effective_sha256
        );
        assert!(!runtime.hardware_applied);
        runtime
            .transition(None, |snapshot| {
                assert!(snapshot.is_none());
                Ok(())
            })
            .unwrap();
        assert!(!runtime.hardware_applied);
        assert_eq!(runtime.active.as_ref().unwrap().bytes, b.bytes);
    }
    #[test]
    fn compensation_deadline_reinitialize_and_disable_are_owner_ordered() {
        let mut runtime = Runtime::default();
        let now = Instant::now();
        let mut task = Compensator::default();
        let mut hw = FakeIo::warm();
        task.poll_runtime(&mut runtime, now, false, &mut hw)
            .unwrap();
        assert_eq!(hw.reads, 0);
        runtime.transition(Some(stock()), |_| Ok(())).unwrap();
        task.poll_runtime(&mut runtime, now, true, &mut hw).unwrap();
        assert_eq!(hw.writes, [(6, 0x42), (7, 2)]);
        assert_eq!(runtime.compensation.table_index, Some(49));
        let reads = hw.reads;
        task.poll_runtime(&mut runtime, now + Duration::from_secs(59), true, &mut hw)
            .unwrap();
        assert_eq!(hw.reads, reads);
        task.poll_runtime(&mut runtime, now + Duration::from_secs(60), true, &mut hw)
            .unwrap();
        assert_eq!(runtime.compensation.reason, "unchanged");
        assert_eq!(hw.writes.len(), 2);
        // Even re-applying the same hash schedules a first check immediately.
        runtime.transition(Some(stock()), |_| Ok(())).unwrap();
        task.poll_runtime(&mut runtime, now + Duration::from_secs(61), true, &mut hw)
            .unwrap();
        assert!(hw.reads > reads + 4);
        let reads = hw.reads;
        runtime.transition(None, |_| Ok(())).unwrap();
        task.poll_runtime(&mut runtime, now + Duration::from_secs(121), true, &mut hw)
            .unwrap();
        assert_eq!(hw.reads, reads);
        let partial = Snapshot::parse(&stock().bytes[..56], CalibrationSource::Firmware).unwrap();
        runtime.transition(Some(partial), |_| Ok(())).unwrap();
        task.poll_runtime(&mut runtime, now, true, &mut hw).unwrap();
        assert_eq!(hw.reads, reads);
        assert_eq!(runtime.compensation.reason, "partial_calibration");
    }
    #[test]
    fn read_failures_and_out_of_range_skip_but_partial_writes_require_reset() {
        for failure in [Some(0x2e), Some(7), None] {
            let mut runtime = Runtime::default();
            runtime.transition(Some(stock()), |_| Ok(())).unwrap();
            let mut task = Compensator::default();
            let mut hw = FakeIo::warm();
            hw.fail_read = failure;
            if failure.is_none() {
                hw.registers.insert(0x2e, 0);
                hw.registers.insert(0x2f, 0);
            }
            task.poll_runtime(&mut runtime, Instant::now(), true, &mut hw)
                .unwrap();
            assert!(hw.writes.is_empty());
            assert!(runtime.hardware_applied);
        }
        let mut runtime = Runtime::default();
        runtime.transition(Some(stock()), |_| Ok(())).unwrap();
        let mut hw = FakeIo::warm();
        hw.fail_write = Some(7);
        let mut task = Compensator::default();
        assert!(task
            .poll_runtime(&mut runtime, Instant::now(), true, &mut hw)
            .is_err());
        assert_eq!(hw.registers[&6], 0x42);
        assert!(!runtime.hardware_applied);
        assert_eq!(runtime.compensation.reason, "write_failed");
        let reads = hw.reads;
        task.poll_runtime(
            &mut runtime,
            Instant::now() + Duration::from_secs(60),
            true,
            &mut hw,
        )
        .unwrap();
        assert_eq!(hw.reads, reads);
        runtime.transition(Some(stock()), |_| Ok(())).unwrap();
        hw.disabled = true;
        assert!(task
            .poll_runtime(&mut runtime, Instant::now(), true, &mut hw)
            .is_err());
        assert_eq!(hw.reads, reads);
        assert!(!runtime.hardware_applied);
        runtime.transition(Some(stock()), |_| Ok(())).unwrap();
        task.poll_runtime(&mut runtime, Instant::now(), false, &mut hw)
            .unwrap();
        assert!(!runtime.hardware_applied);
        assert_eq!(hw.reads, reads);
        // Restoring permission alone never revives a version invalidated by reset.
        hw.disabled = false;
        task.poll_runtime(&mut runtime, Instant::now(), true, &mut hw)
            .unwrap();
        assert_eq!(hw.reads, reads);
    }
    #[test]
    fn explicit_source_does_not_fall_back_and_auto_preserves_priority() {
        let stock = DEFAULT_UX3326_CALIBRATION_DATA;
        let mut upload = stock;
        upload[6] += 1;
        assert_eq!(
            select(CalibrationMode::Auto, Some(&stock), &upload)
                .unwrap()
                .version
                .source,
            CalibrationSource::Factory
        );
        assert_eq!(
            select(CalibrationMode::Firmware, Some(&stock), &upload)
                .unwrap()
                .bytes[6],
            upload[6]
        );
        assert!(select(CalibrationMode::Factory, None, &upload).is_err());
        assert!(select(CalibrationMode::Firmware, Some(&stock), &[0; 288]).is_err());
        let partial = select(CalibrationMode::Firmware, None, &stock[..56]).unwrap();
        assert!(partial.version.supplemented);
        assert!(!partial.version.temperature_compensation_enabled);
        assert_ne!(partial.version.raw_sha256, partial.version.effective_sha256);
    }
    #[test]
    fn stock_temperature_target_and_invalid_tables() {
        let stock =
            Snapshot::parse(&DEFAULT_UX3326_CALIBRATION_DATA, CalibrationSource::Builtin).unwrap();
        assert_eq!(stock.target(0x5980).unwrap(), Some((49, 266)));
        assert_eq!(stock.target(0).unwrap(), None);
        let mut invalid = stock.bytes;
        invalid[0x70] = 4;
        assert!(Snapshot::parse(&invalid, CalibrationSource::Firmware).is_err());
        invalid = stock.bytes;
        invalid[0x42..0x44].fill(0);
        assert!(Snapshot::parse(&invalid, CalibrationSource::Firmware).is_err());
        invalid = stock.bytes;
        invalid[0x60] = 2;
        assert!(Snapshot::parse(&invalid, CalibrationSource::Firmware).is_err());
        invalid = stock.bytes;
        invalid[0x44..0x46].copy_from_slice(&100i16.to_be_bytes());
        let positive = Snapshot::parse(&invalid, CalibrationSource::Firmware).unwrap();
        assert_eq!(positive.target(0).unwrap().unwrap().0, 20);
        invalid[0x40..0x42].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(Snapshot::parse(&invalid, CalibrationSource::Firmware)
            .unwrap()
            .target(u16::MAX)
            .is_err());
    }
}
