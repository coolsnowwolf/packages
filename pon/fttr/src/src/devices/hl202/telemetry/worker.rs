use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

pub(super) const WARM_SEED_LIMIT: usize = 64 * 1024;
const ARCHIVE_BYTES: usize = 1024 * 1024;
const ARCHIVE_SOURCES: usize = 64;
static ARCHIVED_BYTES: AtomicUsize = AtomicUsize::new(0);

pub(super) fn archive_usage() -> (usize, usize) {
    (ARCHIVED_BYTES.load(Relaxed), ARCHIVE_BYTES)
}

struct Archive {
    root: PathBuf,
    files: BTreeMap<String, (u64, usize)>,
    bytes: usize,
}

impl Archive {
    fn open(root: &Path) -> io::Result<Self> {
        let mut archive = Self {
            root: root.into(),
            files: BTreeMap::new(),
            bytes: 0,
        };
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(mac) = name.to_str() else {
                continue;
            };
            if !crate::mqtt_identity::normalize_target(mac).is_some_and(|v| v == mac) {
                continue;
            }
            let file = entry.path().join("telemetry.bin");
            let metadata = match fs::symlink_metadata(&file) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let len = metadata.len();
            if len > WARM_SEED_LIMIT as u64 {
                fs::remove_file(file)?;
                continue;
            }
            let at = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            archive.files.insert(mac.into(), (at, len as usize));
            archive.bytes += len as usize;
            archive.trim(0, 0, None)?;
        }
        ARCHIVED_BYTES.store(archive.bytes, Relaxed);
        Ok(archive)
    }

    fn trim(
        &mut self,
        extra_files: usize,
        extra_bytes: usize,
        keep: Option<&str>,
    ) -> io::Result<()> {
        while self.files.len() + extra_files > ARCHIVE_SOURCES
            || self.bytes + extra_bytes > ARCHIVE_BYTES
        {
            let (mac, &(_, bytes)) = self
                .files
                .iter()
                .filter(|(mac, _)| Some(mac.as_str()) != keep)
                .min_by_key(|(_, (at, _))| *at)
                .ok_or_else(|| io::Error::other("telemetry archive capacity exhausted"))?;
            match fs::remove_file(self.root.join(mac).join("telemetry.bin")) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let mac = mac.clone();
            self.bytes -= bytes;
            self.files.remove(&mac);
            let _ = fs::remove_dir(self.root.join(mac));
        }
        ARCHIVED_BYTES.store(self.bytes, Relaxed);
        Ok(())
    }

    fn save(&mut self, mac: &str, payload: &[u8], at: u64) -> io::Result<()> {
        if payload.len() > WARM_SEED_LIMIT {
            return Ok(());
        }
        let existing = self.files.get(mac).copied();
        // Reserve space for the replacement while the old file still exists.
        self.trim(usize::from(existing.is_none()), payload.len(), Some(mac))?;
        save(&self.root.join(mac), payload)?;
        self.bytes = self.bytes - existing.map_or(0, |(_, bytes)| bytes) + payload.len();
        self.files.insert(mac.into(), (at, payload.len()));
        ARCHIVED_BYTES.store(self.bytes, Relaxed);
        Ok(())
    }
}

fn save(dir: &Path, payload: &[u8]) -> io::Result<()> {
    fs::create_dir_all(&dir)?;
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(io::Error::other("not a telemetry source directory"));
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let temporary = dir.join(format!("telemetry.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(payload)?;
        fs::rename(&temporary, dir.join("telemetry.bin"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
#[test]
fn warm_seed_archive_limits_existing_and_new_files_without_dropping_the_newest() {
    let root = PathBuf::from(crate::provisioning::storage::test_io::registry_directory(
        "/tmp/fttr/warm-seed-test",
    ));
    fs::create_dir_all(&root).unwrap();
    save(&root.join("0200000000FF"), &vec![1; WARM_SEED_LIMIT + 1]).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    assert!(!root.join("0200000000FF/telemetry.bin").exists());
    let payload = vec![7; WARM_SEED_LIMIT];
    for id in 0..40 {
        archive
            .save(&format!("02000000{id:04X}"), &payload, id)
            .unwrap();
    }
    assert_eq!(archive.bytes, ARCHIVE_BYTES);
    assert_eq!(archive.files.len(), ARCHIVE_BYTES / WARM_SEED_LIMIT);
    assert!(!root.join("020000000000/telemetry.bin").exists());
    assert_eq!(
        fs::read(root.join("020000000027/telemetry.bin")).unwrap(),
        payload
    );
}

pub(super) async fn run() {
    let mut archive = None;
    while let Some((mac, payload, received)) = super::store::next().await {
        let work = crate::runtime::spawn_blocking(move || {
            let result = match payload.as_deref() {
                Some(payload) => super::parse_measured(payload)
                    .map_err(|_| "invalid or oversized gzip/tar telemetry".to_owned()),
                None => Err("compressed telemetry limit exceeded".to_owned()),
            };
            if let Ok((report, _)) = &result {
                if let Some((serial, claimed_mac)) = super::identity_claim(report, &mac) {
                    if let Ok(mut identities) = crate::mqtt_identity::shared().lock() {
                        let eval_time = received.max(identities.clock());
                        identities.record_verified_report(&serial, &claimed_mac, eval_time);
                    }
                    if let Some(payload) = payload.as_deref() {
                        // A warm seed is optional. Live identity and decoded reports
                        // must continue to work when this private cache is unavailable.
                        if archive.is_none() {
                            archive = Archive::open(Path::new(crate::ipc::SUBDEV_DIR)).ok();
                        }
                        if let Some(archive) = &mut archive {
                            let _ = archive.save(&mac, payload, received);
                        }
                    }
                }
            }
            super::store::complete(mac, received, result);
            archive
        })
        .await;
        archive = match work {
            Ok(archive) => archive,
            Err(error) => {
                eprintln!("[-] Telemetry parser failed: {error}");
                break;
            }
        };
    }
    eprintln!("[-] Telemetry worker stopped: state unavailable");
}
