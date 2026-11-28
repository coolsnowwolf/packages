//! Validation of the stock 2 KiB optical table, not an optical programming recipe.
use fttr_api::{
    NativeCalibrationApplication, NativeCalibrationInfo, NativeCalibrationTable,
    PlatformCalibrationProfile,
};
use sha2::{Digest, Sha256};
use std::{fmt, io};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const FACTORY_SOURCE: &str = "/dev/ubi0_3";
pub(super) const FACTORY_OFFSET: u64 = 0x1a5000;
static APPLICATION: OnceLock<Mutex<NativeCalibrationApplication>> = OnceLock::new();

fn application() -> &'static Mutex<NativeCalibrationApplication> {
    APPLICATION.get_or_init(|| Mutex::new(Default::default()))
}

pub(super) fn load_factory() -> io::Result<Vec<u8>> {
    let mut file = File::open(FACTORY_SOURCE)?;
    file.seek(SeekFrom::Start(FACTORY_OFFSET))?;
    let mut raw = vec![0; TABLE_BYTES];
    file.read_exact(&mut raw)?;
    validate_factory(&raw)?;
    Ok(raw)
}

fn validate_factory(raw: &[u8]) -> io::Result<()> {
    CalibrationTable::parse(raw)?.require_chip(super::profile::UX3326S_CHIP_ID)
}

fn table_info(raw: io::Result<Vec<u8>>) -> NativeCalibrationTable {
    let mut info = NativeCalibrationTable {
        source: FACTORY_SOURCE.into(),
        offset: FACTORY_OFFSET as u32,
        size: TABLE_BYTES as u32,
        payload_bytes: PAYLOAD_BYTES as u32,
        chip_id: None,
        sha256: None,
        valid: false,
        error: None,
    };
    match raw {
        Ok(bytes) => {
            info.sha256 = Some(format!("{:x}", Sha256::digest(&bytes)));
            match validate_factory(&bytes) {
                Ok(()) => {
                    info.valid = true;
                    info.chip_id = Some(super::profile::UX3326S_CHIP_ID);
                }
                Err(error) => info.error = Some(error.to_string()),
            }
        }
        Err(error) => info.error = Some(error.to_string()),
    }
    info
}

pub(super) fn info() -> NativeCalibrationInfo {
    let factory_table = table_info(load_factory());
    let mut applied = application()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    applied.applied &= factory_table.valid && applied.sha256 == factory_table.sha256;
    NativeCalibrationInfo {
        profile: PlatformCalibrationProfile::Ux3326sFactory,
        can_apply: factory_table.valid,
        can_upload: false,
        can_select: false,
        factory_table,
        application: applied,
    }
}

/// Called only by the optical owner, with downstream disabled and users quiesced.
pub(super) fn apply_factory(mut checkpoint: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    apply_with(
        application(),
        load_factory,
        |raw, checkpoint| super::diagnostics::program_table(raw, checkpoint).map(|_| ()),
        &mut checkpoint,
    )
}

// Inject only the hardware/source boundaries; tests exercise the same receipt
// invalidation and final-deadline policy without accessing calibration devices.
fn apply_with(
    state: &Mutex<NativeCalibrationApplication>,
    load: impl FnOnce() -> io::Result<Vec<u8>>,
    program: impl FnOnce(&[u8], &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    checkpoint: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<()> {
    *state
        .lock()
        .map_err(|_| io::Error::other("calibration state poisoned"))? = Default::default();
    let result = load().and_then(|raw| {
        validate_factory(&raw)?;
        checkpoint()?;
        let sha256 = format!("{:x}", Sha256::digest(&raw));
        program(&raw, checkpoint)?;
        // The final pacing interval may have crossed cancellation/deadline.
        // Do not publish success or permit TX restoration in that case.
        checkpoint()?;
        Ok(NativeCalibrationApplication {
            applied: true,
            sha256: Some(sha256),
            applied_unix: Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .min(u32::MAX as u64) as u32,
            ),
            register_writes: super::profile::UX3326S_REGISTER_WRITES,
            live_signature_verified: true,
            error: None,
        })
    });
    let mut state = state
        .lock()
        .map_err(|_| io::Error::other("calibration state poisoned"))?;
    match result {
        Ok(receipt) => {
            *state = receipt;
            Ok(())
        }
        Err(error) => {
            state.error = Some(error.to_string());
            Err(error)
        }
    }
}

pub const TABLE_BYTES: usize = 0x800;
pub const HEADER_BYTES: usize = 0x80;
pub const PAYLOAD_BYTES: usize = TABLE_BYTES - HEADER_BYTES;

/// Borrows private calibration bytes without logging them. CRC establishes file
/// integrity only; the caller must establish board, optical chip and provenance.
pub struct CalibrationTable<'a> {
    bytes: &'a [u8],
}

impl<'a> CalibrationTable<'a> {
    pub fn parse(bytes: &'a [u8]) -> io::Result<Self> {
        if bytes.len() != TABLE_BYTES {
            return Err(invalid("unsupported optical table length"));
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if word(0x78) != HEADER_BYTES as u32 || word(0x70) != PAYLOAD_BYTES as u32 {
            return Err(invalid("optical table header/payload size mismatch"));
        }
        if !valid_crc(&bytes[..0x7c], word(0x7c)) {
            return Err(invalid("optical table header CRC mismatch"));
        }
        if !valid_crc(&bytes[HEADER_BYTES..], word(0x74)) {
            return Err(invalid("optical table payload CRC mismatch"));
        }
        Ok(Self { bytes })
    }

    /// Raw stock header discriminator. This is not a live chip identification.
    pub fn chip_id(&self) -> u8 {
        self.bytes[0x30]
    }

    pub fn require_chip(&self, independently_identified_chip: u8) -> io::Result<()> {
        if self.chip_id() != independently_identified_chip {
            return Err(invalid("optical table/chip discriminator mismatch"));
        }
        Ok(())
    }

    /// Opaque data for a future chip-specific consumer; not a register dump.
    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[HEADER_BYTES..]
    }
}

impl fmt::Debug for CalibrationTable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalibrationTable")
            .field("chip_id", &self.chip_id())
            .field("payload_bytes", &PAYLOAD_BYTES)
            .finish_non_exhaustive()
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_crc(bytes: &[u8], expected: u32) -> bool {
    // The original validator accepts these seeds independently for each region.
    crc32(bytes, 0) == expected || crc32(bytes, u32::MAX) == expected
}

fn crc32(bytes: &[u8], seed: u32) -> u32 {
    let mut crc = !seed;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests;
