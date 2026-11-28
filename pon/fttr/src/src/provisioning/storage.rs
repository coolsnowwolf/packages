//! Persistence mechanics only; callers retain format and retry decisions.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) fn registry_path(directory: &str, id: &str) -> String {
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    #[cfg(test)]
    let directory = test_io::registry_directory(directory);
    format!("{directory}/{hash:016x}.json")
}

pub(crate) fn remove_registry(path: &str) -> Result<(), String> {
    if let Some(key) = path.strip_prefix(super::runtime_store::PREFIX) {
        return super::runtime_store::remove(key).map_err(|e| e.to_string());
    }
    match fs::remove_file(path) {
        Ok(()) => fs::File::open(
            Path::new(path)
                .parent()
                .ok_or("state path has no directory")?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("persist registry removal: {e}")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let directory = Path::new(path)
                .parent()
                .ok_or("state path has no directory")?;
            match fs::File::open(directory) {
                Ok(directory) => directory
                    .sync_all()
                    .map_err(|e| format!("persist registry removal: {e}")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("open registry directory: {e}")),
            }
        }
        Err(e) => Err(format!("remove ownership registry: {e}")),
    }
}

fn sync_directories(directory: &Path) -> io::Result<()> {
    // create_dir_all may have created several ancestors. Retrying after a
    // failed barrier must also make those directory entries durable.
    for ancestor in directory.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        #[cfg(test)]
        test_io::barrier(test_io::Point::DirectorySync)?;
        fs::File::open(ancestor)?.sync_all()?;
    }
    Ok(())
}

fn sync_file(file: &fs::File) -> io::Result<()> {
    #[cfg(test)]
    test_io::barrier(test_io::Point::FileSync)?;
    file.sync_all()
}

/// Avoid rewriting identical snapshots. Keep durability barriers even on the
/// identical-content path: a previous attempt may have renamed successfully but
/// failed to sync the directory. Retrying must finish that commit, not assume it.
pub(crate) fn write_atomic(path: &str, data: &[u8], sync_directory: bool) -> io::Result<()> {
    if let Some(key) = path.strip_prefix(super::runtime_store::PREFIX) {
        return super::runtime_store::write(key, data);
    }
    let directory = Path::new(path).parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state path has no directory")
    })?;
    if let Ok(mut existing) = fs::File::open(path) {
        let mut bytes = Vec::new();
        if existing.read_to_end(&mut bytes).is_ok() && bytes == data {
            if existing.metadata()?.permissions().mode() & 0o777 != 0o600 {
                existing.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            sync_file(&existing)?;
            if sync_directory {
                sync_directories(directory)?;
            }
            return Ok(());
        }
    }
    fs::create_dir_all(directory)?;
    let temporary = format!("{path}.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    sync_file(&file)?;
    #[cfg(test)]
    test_io::barrier(test_io::Point::Rename)?;
    fs::rename(temporary, path)?;
    if sync_directory {
        sync_directories(directory)?;
    }
    Ok(())
}

// Tests run the production save/rename/fsync path in a per-test-thread sandbox.
// Only the directory and fault injection points differ; no save is stubbed out.
#[cfg(test)]
pub(crate) mod test_io {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Point {
        FileSync,
        Rename,
        DirectorySync,
    }

    struct Sandbox(PathBuf);
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    thread_local! {
        static DIRECTORY: Sandbox = Sandbox(std::env::temp_dir().join(format!("fttr-registries-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos())));
        static FAILURE: Cell<Option<Point>> = const { Cell::new(None) };
        static EVENTS: RefCell<Vec<Point>> = const { RefCell::new(Vec::new()) };
    }
    pub fn registry_directory(directory: &str) -> String {
        if !directory.starts_with("/etc/fttr/") && !directory.starts_with("/tmp/fttr/") {
            return directory.to_owned();
        }
        DIRECTORY.with(|root| {
            root.0
                .join(directory.trim_start_matches('/'))
                .to_string_lossy()
                .into_owned()
        })
    }
    pub fn fail_next(point: Point) {
        FAILURE.with(|f| f.set(Some(point)));
    }
    pub fn events() -> Vec<Point> {
        EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
    }
    pub fn barrier(point: Point) -> io::Result<()> {
        EVENTS.with(|e| e.borrow_mut().push(point));
        FAILURE.with(|f| {
            if f.get() == Some(point) {
                f.set(None);
                Err(io::Error::other("injected durability barrier failure"))
            } else {
                Ok(())
            }
        })
    }
}

pub(crate) fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    if let Some(key) = path
        .to_str()
        .and_then(|s| s.strip_prefix(super::runtime_store::PREFIX))
    {
        return super::runtime_store::read(key);
    }
    fs::read(path)
}

pub(crate) fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn repeated_snapshot_keeps_inode_and_changed_snapshot_is_atomic() {
        let directory = std::env::temp_dir().join(format!(
            "fttr-storage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("state.json");
        let path = path.to_str().unwrap();
        write_atomic(path, b"pending", true).unwrap();
        let original = fs::File::open(path).unwrap();
        let metadata = original.metadata().unwrap();
        for _ in 0..10 {
            write_atomic(path, b"pending", true).unwrap();
        }
        let unchanged = fs::metadata(path).unwrap();
        assert_eq!(unchanged.ino(), metadata.ino());
        assert_eq!(unchanged.mtime(), metadata.mtime());
        assert_eq!(unchanged.mtime_nsec(), metadata.mtime_nsec());
        assert!(!Path::new(&format!("{path}.tmp")).exists());
        write_atomic(path, b"completed", true).unwrap();
        assert_ne!(fs::metadata(path).unwrap().ino(), metadata.ino());
        assert_eq!(fs::read(path).unwrap(), b"completed");
        let mut before_commit = String::new();
        (&original).read_to_string(&mut before_commit).unwrap();
        assert_eq!(before_commit, "pending");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
