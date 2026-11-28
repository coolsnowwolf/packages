//! UX3326 calibration API.
use base64::{engine::general_purpose::STANDARD, Engine};
use fttr_api::{
    CalibrationInfo, CalibrationSaved, CalibrationSource, FactoryCalibration, FirmwareCalibration,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
};

const PATH: &str = "/lib/firmware/modified_UX3326.bin";
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn get() -> io::Result<CalibrationInfo> {
    let (device, factory) = bosa::factory_calibration();
    let firmware_result = optical_calibration::firmware();
    let firmware = firmware_result
        .as_ref()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mode = crate::optical::load_calibration_mode()?;
    let candidate = if mode == fttr_api::CalibrationMode::Firmware && firmware_result.is_err() {
        Err(io::Error::other(
            firmware_result.as_ref().unwrap_err().to_string(),
        ))
    } else {
        optical_calibration::select(mode, factory.as_ref().map(|v| v.as_slice()), firmware)
    };
    let state = optical_calibration::state(
        mode,
        candidate
            .as_ref()
            .map_err(|e| io::Error::other(e.to_string())),
    );
    let exists = std::path::Path::new(PATH).is_file();
    // Legacy fields describe the candidate, never masquerade as the applied version.
    // An invalid candidate is represented by empty legacy data plus candidate_error.
    let source = candidate
        .as_ref()
        .map(|s| s.version.source)
        .unwrap_or(CalibrationSource::Builtin);
    let bytes = candidate
        .as_ref()
        .map(|s| s.bytes.as_slice())
        .unwrap_or_default();
    let path = match source {
        CalibrationSource::Factory => "Factory partition (offset 0x2200)",
        CalibrationSource::Firmware => PATH,
        CalibrationSource::Builtin => "Builtin historical fallback",
    };
    Ok(CalibrationInfo {
        state: Some(state),
        source,
        active_path: path.into(),
        size: bytes.len() as u32,
        sha256: hash(bytes),
        data: STANDARD.encode(bytes),
        factory: FactoryCalibration {
            detected: device.is_some(),
            device: device.unwrap_or_default(),
            has_data: factory.is_some(),
            sha256: factory.as_ref().map(|b| hash(b)).unwrap_or_default(),
            data: factory
                .as_ref()
                .map(|b| STANDARD.encode(b))
                .unwrap_or_default(),
        },
        firmware: FirmwareCalibration {
            exists,
            path: PATH.into(),
            size: firmware.len() as u32,
            sha256: if exists {
                hash(&firmware)
            } else {
                String::new()
            },
            data: STANDARD.encode(firmware),
        },
    })
}

pub(super) fn save(data: &str) -> io::Result<CalibrationSaved> {
    save_at(data, std::path::Path::new(PATH))
}

fn save_at(data: &str, path: &std::path::Path) -> io::Result<CalibrationSaved> {
    if data.len() > 384 {
        return Err(io::Error::other("Invalid calibration length"));
    }
    let bytes = STANDARD.decode(data).map_err(io::Error::other)?;
    if !matches!(bytes.len(), 56 | 288) {
        return Err(io::Error::other(
            "Calibration payload must be 56 or 288 bytes",
        ));
    }
    optical_calibration::Snapshot::parse(&bytes, CalibrationSource::Firmware)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid calibration path"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(CalibrationSaved {
            success: true,
            path: path.to_string_lossy().into_owned(),
            size: bytes.len() as u32,
            sha256: hash(&bytes),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_replacement_validates_before_writing_and_preserves_foreign_temporary_files() {
        let directory =
            std::env::temp_dir().join(format!("fttr-calibration-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("calibration.bin");
        for length in [56, 288] {
            let bytes = bosa::DEFAULT_UX3326_CALIBRATION_DATA[..length].to_vec();
            let reply = save_at(&STANDARD.encode(&bytes), &path).unwrap();
            assert_eq!(reply.sha256, hash(&bytes));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        let expected = fs::read(&path).unwrap();
        for input in [
            "invalid-base64".to_owned(),
            STANDARD.encode([0; 57]),
            STANDARD.encode([0; 289]),
        ] {
            assert!(save_at(&input, &path).is_err());
            assert_eq!(fs::read(&path).unwrap(), expected);
        }
        let temporary = directory.join(format!("calibration.bin.{}.tmp", std::process::id()));
        fs::write(&temporary, b"existing temporary data").unwrap();
        assert!(save_at(
            &STANDARD.encode(bosa::DEFAULT_UX3326_CALIBRATION_DATA),
            &path
        )
        .is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"existing temporary data");
        assert_eq!(fs::read(&path).unwrap(), expected);
        fs::remove_file(&temporary).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(save_at(
            &STANDARD.encode(bosa::DEFAULT_UX3326_CALIBRATION_DATA),
            &path
        )
        .is_err());
        assert!(!temporary.exists());
        assert!(path.is_dir());
        fs::remove_dir_all(directory).unwrap();
    }
}
use crate::platform::h3c_hm2004_du::{calibration as optical_calibration, fmcs as bosa};
