//! Bounded script input; shell interpretation belongs exclusively to the ONU.

use std::io;
use std::io::Read;
use std::path::Path;

pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

pub fn read_file(path: &Path) -> io::Result<String> {
    validate(read_text_file(path, MAX_SCRIPT_BYTES)?)
}

pub(crate) fn read_text_file(path: &Path, max_bytes: usize) -> io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Linux O_NONBLOCK prevents a replaced request path from blocking on a FIFO.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x800)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input must be a regular file",
        ));
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid input byte limit"))?;
    file.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input exceeds the byte limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "input must be UTF-8"))
}

pub fn validate(source: String) -> io::Result<String> {
    if source.len() > MAX_SCRIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "script exceeds 65536 bytes",
        ));
    }
    if source.trim().is_empty() || source.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "script must be nonempty and contain no NUL",
        ));
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mqtt_command_script_file_is_read_as_utf8_data() {
        let path = std::env::temp_dir().join(format!("fttr-script-{}", std::process::id()));
        let source = "printf '中文';\necho $(id) `uname`";
        std::fs::write(&path, source).unwrap();
        let parsed = read_file(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(parsed.unwrap(), source);
        assert!(read_file(&std::env::temp_dir()).is_err());
    }

    #[test]
    fn mqtt_command_script_preserves_arbitrary_remote_shell() {
        // Given a multiline script containing shell syntax and Unicode.
        let source = "printf '%s\\n' '中文'; echo $(id) `uname`;\nexit 7";
        // When the input boundary parses it.
        let parsed = validate(source.to_string()).unwrap();
        // Then the exact script remains data.
        assert_eq!(parsed, source);
    }

    #[test]
    fn mqtt_command_script_rejects_empty_nul_and_limit_plus_one() {
        // Given invalid inputs and the exact protocol boundary.
        for source in [
            "".to_string(),
            " \n".into(),
            "echo\0x".into(),
            "x".repeat(65_537),
        ] {
            // When parsing; then each invalid input is rejected.
            assert!(validate(source).is_err());
        }
        assert_eq!(validate("x".repeat(65_536)).unwrap().len(), 65_536);
    }
}
