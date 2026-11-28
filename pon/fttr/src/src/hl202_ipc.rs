use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;

/// Preserve the wire-size budget for typed callers without allocating a JSON body.
pub fn check_request_size(value: &impl serde::Serialize) -> io::Result<()> {
    struct Budget(usize);
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "HL202 request exceeds the byte limit",
                )
            })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(MAX_REQUEST_BYTES), value).map_err(io::Error::other)
}

pub fn send(body: &str, timeout: Duration) -> io::Result<String> {
    exchange(UnixStream::connect(crate::ipc::SOCKET_PATH)?, body, timeout)
}

fn exchange(mut stream: UnixStream, body: &str, timeout: Duration) -> io::Result<String> {
    if body.len() > MAX_REQUEST_BYTES || body.contains(['\n', '\r']) || timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid HL202 IPC envelope",
        ));
    }
    let deadline = std::time::Instant::now() + timeout;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"HL202 ")?;
    stream.write_all(body.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HL202 IPC deadline exceeded",
            ));
        }
        stream.set_read_timeout(Some(remaining))?;
        let capacity = buffer.len().min(MAX_RESPONSE_BYTES + 1 - response.len());
        let count = match stream.read(&mut buffer[..capacity]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        if response.len() + count > MAX_RESPONSE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HL202 IPC response exceeds the byte limit",
            ));
        }
        response.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(response).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HL202 IPC response is not UTF-8",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::thread;

    #[test]
    fn typed_request_budget_counts_json_escaping_and_accepts_exact_limit() {
        assert!(check_request_size(&"a".repeat(MAX_REQUEST_BYTES - 2)).is_ok());
        assert!(check_request_size(&"a".repeat(MAX_REQUEST_BYTES - 1)).is_err());
        // The wire budget counts encoded bytes, not only the source string.
        assert!(check_request_size(&"\n".repeat(MAX_REQUEST_BYTES / 2)).is_err());
    }

    #[test]
    fn hl202_ipc_transports_multiline_script_as_json_without_interpretation() {
        // Given a private socket peer and JSON containing remote shell syntax.
        let (client, mut server) = UnixStream::pair().unwrap();
        let body = serde_json::json!({"script":"猫\n$(touch /tmp/never-local); `id`"}).to_string();
        let expected = format!("HL202 {body}\n");
        let worker = thread::spawn(move || {
            let mut wire = String::new();
            server.read_to_string(&mut wire).unwrap();
            assert_eq!(wire, expected);
            server.write_all("{\"output\":\"猫\"}".as_bytes()).unwrap();
        });
        // When the bounded client exchanges one request.
        let response = exchange(client, &body, Duration::from_secs(2)).unwrap();
        // Then the exact JSON response survives UTF-8 transport.
        assert_eq!(response, "{\"output\":\"猫\"}");
        worker.join().unwrap();
    }

    #[test]
    fn hl202_ipc_rejects_response_limit_plus_one() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut input = String::new();
            server.read_to_string(&mut input).unwrap();
            let _ = server.write_all(&vec![b' '; MAX_RESPONSE_BYTES + 1]);
            let _ = server.shutdown(Shutdown::Write);
        });
        let error = exchange(client, "{}", Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        worker.join().unwrap();
    }
}
