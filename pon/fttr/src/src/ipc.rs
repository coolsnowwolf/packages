use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const SOCKET_PATH: &str = "/var/run/fttrd.sock";
pub const STATUS_FILE: &str = "/tmp/fttr_status.json";
pub const SUBDEV_DIR: &str = "/tmp/fttr_subdev";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubDevice {
    pub id: String,
    pub last_seen: u64,
    pub payload_bytes: usize,
}

#[derive(Serialize)]
struct StatusCacheDev {
    id: String,
    last_seen_sec_ago: u64,
    last_payload_bytes: usize,
}

#[derive(Serialize)]
struct StatusCache {
    updated_at: u64,
    devices: Vec<StatusCacheDev>,
}

pub fn create_socket_listener() -> io::Result<UnixListener> {
    if Path::new(SOCKET_PATH).exists() {
        let _ = fs::remove_file(SOCKET_PATH);
    }
    UnixListener::bind(SOCKET_PATH)
}

pub fn send_ipc_command(cmd: &str) -> io::Result<String> {
    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    if cmd.starts_with("OPTICAL ") || cmd.starts_with("FPGA_DIAG ") {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(8)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(1)))?;
    }
    stream.write_all(format!("{cmd}\n").as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

pub fn update_status_file(devices: &[SubDevice]) -> io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cache = StatusCache {
        updated_at: now,
        devices: devices
            .iter()
            .map(|dev| StatusCacheDev {
                id: dev.id.clone(),
                last_seen_sec_ago: now.saturating_sub(dev.last_seen),
                last_payload_bytes: dev.payload_bytes,
            })
            .collect(),
    };

    let json = serde_json::to_string_pretty(&cache)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(STATUS_FILE, json)
}
