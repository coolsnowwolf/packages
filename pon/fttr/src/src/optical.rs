//! Optical desired state and process ownership. Hardware transitions belong to the PLOAM owner.
use crate::config::UciSession;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;

pub const OWNER_LOCK: &str = "/var/run/fttr-optical.lock";

/// Held for the daemon lifetime; an offline shutdown must never race a live owner.
pub fn claim_owner() -> io::Result<File> {
    claim_owner_at(std::path::Path::new(OWNER_LOCK))
}

fn claim_owner_at(path: &std::path::Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "optical hardware is owned by fttrd; shutdown outcome unknown",
        ));
    }
    Ok(file)
}

const CONFIG: &str = "/etc/config/fttr";

pub fn settings() -> fttr_api::OpticalSettings {
    cache().read().unwrap().settings
}

pub fn fec_generation() -> u64 {
    cache().read().unwrap().generation
}

struct Configuration {
    settings: fttr_api::OpticalSettings,
    generation: u64,
}

pub fn fec_enabled() -> bool {
    let settings = settings();
    settings.enabled && settings.downstream_tx_enabled && settings.fec_enabled
}

fn cache() -> &'static std::sync::RwLock<Configuration> {
    static CACHE: std::sync::OnceLock<std::sync::RwLock<Configuration>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::RwLock::new(Configuration {
            generation: 0,
            settings: fttr_api::OpticalSettings {
                enabled: false,
                downstream_tx_enabled: false,
                fec_enabled: false,
            },
        })
    })
}

pub fn refresh() -> io::Result<fttr_api::OpticalSettings> {
    let settings = load_settings_from(std::path::Path::new(CONFIG));
    let next = settings
        .as_ref()
        .copied()
        .unwrap_or(fttr_api::OpticalSettings {
            enabled: false,
            downstream_tx_enabled: false,
            fec_enabled: false,
        });
    let mut state = cache().write().unwrap();
    if state.settings != next {
        state.generation = state.generation.wrapping_add(1);
        state.settings = next;
    }
    settings
}

impl UciSession {
    fn load(&mut self) -> io::Result<fttr_api::OpticalSettings> {
        let view = crate::config::export(&mut self.uci, &self.package).map_err(io::Error::other)?;
        settings_from_view(&view)
    }

    fn set_optical(&mut self, settings: fttr_api::OpticalSettings) -> io::Result<()> {
        self.uci
            .ensure_section(&self.package, "optical", "optical")
            .map_err(|e| io::Error::other(e.to_string()))?;
        for (key, value) in [
            ("enabled", settings.enabled),
            ("downstream_tx_enabled", settings.downstream_tx_enabled),
            ("fec_enabled", settings.fec_enabled),
        ] {
            self.uci
                .set(&self.package, "optical", key, if value { "1" } else { "0" })
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        self.uci
            .commit(&self.package)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    }
}
pub fn load_enabled() -> io::Result<bool> {
    refresh().map(|settings| settings.enabled)
}

pub fn load_enabled_from(config_path: &std::path::Path) -> io::Result<bool> {
    load_settings_from(config_path).map(|settings| settings.enabled)
}

pub fn load_settings_from(config_path: &std::path::Path) -> io::Result<fttr_api::OpticalSettings> {
    if !config_path.exists() {
        return parse_settings("");
    }
    UciSession::new(config_path)?.load()
}

pub fn save_enabled(enabled: bool) -> io::Result<()> {
    save_at(std::path::Path::new(CONFIG), enabled)
}

pub fn save(settings: fttr_api::OpticalSettings) -> io::Result<()> {
    save_settings_at(std::path::Path::new(CONFIG), Some(settings), None)
}

pub fn load_calibration_mode() -> io::Result<fttr_api::CalibrationMode> {
    let config = std::path::Path::new(CONFIG);
    if !config.exists() {
        return Ok(fttr_api::CalibrationMode::Auto);
    }
    let view = crate::config::committed(config).map_err(io::Error::other)?;
    calibration_from_view(&view)
}

#[cfg(test)]
pub(crate) fn parse_calibration_mode(text: &str) -> io::Result<fttr_api::CalibrationMode> {
    calibration_from_view(&crate::config::View::parse(text).map_err(io::Error::other)?)
}
pub(crate) fn calibration_from_view(
    view: &crate::config::View,
) -> io::Result<fttr_api::CalibrationMode> {
    let mut seen = false;
    let mut mode = fttr_api::CalibrationMode::Auto;
    let mut source_seen = false;
    for section in &view.sections {
        if section.kind == "calibration" {
            if seen || section.name.as_deref() != Some("calibration") {
                return Err(io::Error::other("expected one named calibration section"));
            }
            seen = true;
        } else if section.name.as_deref() == Some("calibration") {
            return Err(io::Error::other(
                "fttr.calibration has the wrong section type",
            ));
        }
        if section.kind == "calibration" {
            for option in &section.options {
                if source_seen || option.is_list || option.key != "source" {
                    return Err(io::Error::other("invalid calibration source option"));
                }
                source_seen = true;
                mode = serde_json::from_value(serde_json::Value::String(option.value.clone()))
                    .map_err(io::Error::other)?;
            }
        }
    }
    Ok(mode)
}

pub fn save_calibration_mode(mode: fttr_api::CalibrationMode) -> io::Result<()> {
    let config = std::path::Path::new(CONFIG);
    let source = OpenOptions::new().read(true).write(true).open(config)?;
    if unsafe { libc::flock(source.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "FTTR configuration is being edited; retry",
        ));
    }
    let mut session = UciSession::new(config)?;
    session
        .uci
        .ensure_section(&session.package, "calibration", "calibration")
        .map_err(|e| io::Error::other(e.to_string()))?;
    let value = serde_json::to_value(mode).map_err(io::Error::other)?;
    session
        .uci
        .set(
            &session.package,
            "calibration",
            "source",
            value.as_str().unwrap(),
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
    session
        .uci
        .commit(&session.package)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let bytes = fs::read(session.conf_dir.join(&session.package))?;
    let view = crate::config::committed(&session.conf_dir.join(&session.package))
        .map_err(io::Error::other)?;
    if calibration_from_view(&view)? != mode {
        return Err(io::Error::other("calibration source readback mismatch"));
    }
    replace_config(config, &source, &session.original, &bytes, &session.package)
}

fn save_at(config: &std::path::Path, enabled: bool) -> io::Result<()> {
    save_settings_at(config, None, Some(enabled))
}

fn save_settings_at(
    config: &std::path::Path,
    settings: Option<fttr_api::OpticalSettings>,
    enabled: Option<bool>,
) -> io::Result<()> {
    let source = OpenOptions::new().read(true).write(true).open(config)?;
    if unsafe { libc::flock(source.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "FTTR configuration is being edited; retry",
        ));
    }
    let mut session = UciSession::new(config)?;
    let current = session.load()?;
    let mut settings = settings.unwrap_or(current);
    if let Some(enabled) = enabled {
        settings.enabled = enabled;
    }
    if current == settings {
        return Ok(());
    }
    session.set_optical(settings)?;
    if session.load()? != settings {
        return Err(io::Error::other("optical configuration readback mismatch"));
    }
    let bytes = fs::read(session.conf_dir.join(&session.package))?;
    replace_config(config, &source, &session.original, &bytes, &session.package)
}

fn replace_config(
    config: &std::path::Path,
    source: &File,
    original: &[u8],
    bytes: &[u8],
    token: &str,
) -> io::Result<()> {
    let staged = config.with_file_name(format!(".{token}.tmp"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    let result = (|| {
        let meta = source.metadata()?;
        if unsafe { libc::fchown(output.as_raw_fd(), meta.uid(), meta.gid()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        output.set_permissions(meta.permissions())?;
        output.write_all(bytes)?;
        output.sync_all()?;
        if fs::read(config)? != original {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "FTTR configuration changed during optical save; retry",
            ));
        }
        fs::rename(&staged, config)?;
        File::open(
            config
                .parent()
                .ok_or_else(|| io::Error::other("configuration has no parent"))?,
        )?
        .sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(staged);
    result?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn parse_export(text: &str) -> io::Result<bool> {
    parse_settings(text).map(|settings| settings.enabled)
}

pub(crate) fn parse_settings(text: &str) -> io::Result<fttr_api::OpticalSettings> {
    settings_from_view(&crate::config::View::parse(text).map_err(io::Error::other)?)
}
pub(crate) fn settings_from_view(
    view: &crate::config::View,
) -> io::Result<fttr_api::OpticalSettings> {
    let mut seen = false;
    let mut settings = fttr_api::OpticalSettings {
        enabled: true,
        downstream_tx_enabled: true,
        fec_enabled: false,
    };
    for section in &view.sections {
        let name = section.name.as_deref();
        if section.kind == "optical" {
            if seen || name != Some("optical") {
                return Err(io::Error::other(
                    "expected one named config optical 'optical' section",
                ));
            }
            seen = true;
        } else if name == Some("optical") {
            return Err(io::Error::other("fttr.optical has the wrong section type"));
        }
        for option in &section.options {
            if section.kind == "provisioning"
                && matches!(
                    option.key.as_str(),
                    "fec_enabled" | "downstream_tx_enabled" | "downstream_fec_enabled"
                )
            {
                return Err(io::Error::other(
                    "optical controls must be configured in fttr.optical, not provisioning",
                ));
            }
            if section.kind == "optical" {
                let key = option.key.as_str();
                let value = match (option.is_list, option.value.as_str()) {
                    (false, "1") => true,
                    (false, "0") => false,
                    _ => {
                        return Err(io::Error::other(format!(
                            "fttr.optical.{key} must be 0 or 1"
                        )))
                    }
                };
                match key {
                    "enabled" => settings.enabled = value,
                    "downstream_tx_enabled" => settings.downstream_tx_enabled = value,
                    "fec_enabled" => settings.fec_enabled = value,
                    _ => {
                        return Err(io::Error::other(format!(
                            "unsupported optical option {key}"
                        )))
                    }
                }
            }
        }
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    #[test]
    fn calibration_source_is_separate_from_optical_flags_and_invalid_sources_fail() {
        use fttr_api::CalibrationMode;
        assert_eq!(
            super::parse_calibration_mode("").unwrap(),
            CalibrationMode::Auto
        );
        for source in ["auto", "factory", "firmware", "builtin"] {
            let text = format!("config calibration 'calibration'\n option source '{source}'\nconfig optical 'optical'\n option enabled '0'\n");
            assert!(super::parse_calibration_mode(&text).is_ok());
            assert!(!super::parse_settings(&text).unwrap().enabled);
        }
        assert!(super::parse_calibration_mode(
            "config calibration 'calibration'\n option source 'other'"
        )
        .is_err());
        assert!(super::parse_calibration_mode(
            "config calibration 'calibration'\n option source 'auto'\n option source 'factory'"
        )
        .is_err());
    }
    use super::*;
    #[test]
    fn optical_defaults_and_strict_settings() {
        assert!(parse_export("package fttr\nconfig acs 'acs'\n option enabled '0'\n").unwrap());
        assert!(!parse_export("config optical 'optical'\n option enabled '0'\n").unwrap());
        assert!(parse_export("config optical 'optical'\n option enabled '1'\n").unwrap());
        for text in [
            "config optical 'other'",
            "config acs 'optical'",
            "config optical 'optical'\noption enabled 'yes'",
            "config optical 'optical'\nlist enabled '1'",
            "config optical 'optical'\nconfig optical 'optical'",
            "config optical 'optical'\noption downstream_fec_enabled '1'",
            "config provisioning 'provisioning'\noption fec_enabled '1'",
            "config provisioning 'provisioning'\noption downstream_tx_enabled '0'",
            "config provisioning 'provisioning'\noption downstream_fec_enabled '1'",
        ] {
            assert!(parse_export(text).is_err(), "{text}");
        }
    }

    #[test]
    fn grouped_optical_controls_keep_subordinate_intent_and_defaults() {
        let defaults = parse_settings("").unwrap();
        assert!(defaults.enabled && defaults.downstream_tx_enabled && !defaults.fec_enabled);
        for mask in 0..8 {
            let text = format!("config optical 'optical'\noption enabled '{}'\noption downstream_tx_enabled '{}'\noption fec_enabled '{}'\n", mask & 1, (mask >> 1) & 1, (mask >> 2) & 1);
            let settings = parse_settings(&text).unwrap();
            assert_eq!(settings.enabled, mask & 1 != 0);
            assert_eq!(settings.downstream_tx_enabled, mask & 2 != 0);
            assert_eq!(settings.fec_enabled, mask & 4 != 0);
        }
        for key in ["enabled", "downstream_tx_enabled", "fec_enabled"] {
            assert!(
                parse_settings(&format!("config optical 'optical'\noption {key} 'yes'\n")).is_err()
            );
        }
    }

    #[test]
    fn optical_save_round_trips_through_libuci_and_preserves_other_sections() {
        let dir = std::env::temp_dir().join(format!("fttr-optical-uci-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("fttr");
        fs::write(&path, "config acs 'acs'\n option password 'test-value'\nconfig optical 'optical'\n option enabled '1'\n").unwrap();
        save_at(&path, false).unwrap();
        assert!(!load_settings_from(&path).unwrap().enabled);
        let view = crate::config::committed(&path).unwrap();
        assert_eq!(
            view.option("acs", "password").as_deref(),
            Some("test-value")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_config_write_preserves_metadata_and_rejects_concurrent_edits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fttr-optical-atomic-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let config = dir.join("fttr");
        let original = b"config acs 'acs'\n option password 'unchanged'\n";
        fs::write(&config, original).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let source = File::open(&config).unwrap();
        let next = b"config acs 'acs'\n option password 'unchanged'\nconfig optical 'optical'\n option enabled '0'\n";
        replace_config(&config, &source, original, next, "optical-test").unwrap();
        assert_eq!(fs::read(&config).unwrap(), next);
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            replace_config(&config, &source, original, b"stale writer", "optical-test").is_err()
        );
        assert_eq!(fs::read(&config).unwrap(), next);
        assert!(!dir.join(".optical-test.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn offline_shutdown_cannot_claim_live_owner_and_drop_releases_lock() {
        let path =
            std::env::temp_dir().join(format!("fttr-optical-lock-test-{}", std::process::id()));
        let owner = claim_owner_at(&path).unwrap();
        assert_eq!(
            claim_owner_at(&path).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(owner);
        drop(claim_owner_at(&path).unwrap());
        fs::remove_file(path).unwrap();
    }
}
