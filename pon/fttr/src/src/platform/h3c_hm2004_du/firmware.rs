//! HM2004-DU FPGA firmware lifecycle.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

const SYSFS_RELOAD_PATHS: &[&str] = &[
    "/sys/devices/platform/1fa10000.fmcs/reload_fpga",
    "/sys/devices/platform/fmcs/reload_fpga",
];

const SYSFS_STATUS_PATHS: &[&str] = &[
    "/sys/devices/platform/1fa10000.fmcs/fpga_status",
    "/sys/devices/platform/fmcs/fpga_status",
];

const GPIO_CLK: u32 = 29;
const GPIO_DATA: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FpgaStatus {
    NotStarted,
    Programming,
    Ready,
    Failed,
    Unknown,
}

impl std::fmt::Display for FpgaStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FpgaStatus::NotStarted => write!(f, "not_started"),
            FpgaStatus::Programming => write!(f, "programming"),
            FpgaStatus::Ready => write!(f, "ready"),
            FpgaStatus::Failed => write!(f, "failed"),
            FpgaStatus::Unknown => write!(f, "unknown"),
        }
    }
}

pub fn get_fpga_status() -> FpgaStatus {
    for path in SYSFS_STATUS_PATHS {
        if let Ok(content) = std::fs::read_to_string(path) {
            match content.trim() {
                "programming" => return FpgaStatus::Programming,
                "ready" => return FpgaStatus::Ready,
                "failed" => return FpgaStatus::Failed,
                "not_started" => return FpgaStatus::NotStarted,
                _ => {}
            }
        }
    }

    if Path::new("/dev/fmcs_mci").exists() {
        if let Ok(ts) = super::fmcs::read_fpga_reg(0x00000008) {
            if ts != 0 && ts != 0xFFFFFFFF {
                return FpgaStatus::Ready;
            }
        }
    }

    FpgaStatus::Unknown
}

pub fn is_driver_loaded() -> bool {
    Path::new("/dev/fmcs_mci").exists() || SYSFS_RELOAD_PATHS.iter().any(|p| Path::new(p).exists())
}

pub fn load_bitstream(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Bitstream file not found: {}", path.display()),
        ));
    }

    let cur_status = get_fpga_status();
    if cur_status == FpgaStatus::Programming {
        println!("[*] FPGA bitstream is already being programmed in background, please wait.");
        return Ok(());
    }

    // Fast path 1: Check if FPGA is already programmed and active
    if let Ok(ts) = super::fmcs::read_fpga_reg(0x00000008) {
        if ts != 0 && ts != 0xFFFFFFFF {
            println!(
                "[+] FPGA bitstream already active (timestamp: 0x{:08X}), skipping reload.",
                ts
            );
            return Ok(());
        }
    }

    // Fast path 2: Trigger high-speed kernel bitstream loader via sysfs
    for reload_path in SYSFS_RELOAD_PATHS {
        if Path::new(reload_path).exists() {
            println!("[+] Found active h3c-fmcs driver, triggering kernel async reload...");
            let mut f = OpenOptions::new().write(true).open(reload_path)?;
            f.write_all(b"1\n")?;
            println!("[+] FPGA bitstream reload successfully triggered in background.");
            return Ok(());
        }
    }

    println!(
        "[*] In-kernel loader not available; performing user-space Passive Serial programming..."
    );
    export_gpio(GPIO_CLK)?;
    export_gpio(GPIO_DATA)?;
    sleep(Duration::from_millis(50));

    set_gpio_direction(GPIO_CLK, "out")?;
    set_gpio_direction(GPIO_DATA, "out")?;

    let mut clk_file = OpenOptions::new()
        .write(true)
        .open(format!("/sys/class/gpio/gpio{GPIO_CLK}/value"))?;
    let mut data_file = OpenOptions::new()
        .write(true)
        .open(format!("/sys/class/gpio/gpio{GPIO_DATA}/value"))?;

    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    println!(
        "[*] Clocking {} bytes into FPGA via GPIO {}/{}...",
        buffer.len(),
        GPIO_CLK,
        GPIO_DATA
    );

    for byte in buffer {
        for bit in (0..8).rev() {
            let bit_val = if (byte >> bit) & 1 == 1 { b"1" } else { b"0" };
            let _ = data_file.write_all(bit_val);
            let _ = clk_file.write_all(b"1");
            let _ = clk_file.write_all(b"0");
        }
    }

    println!("[+] User-space FPGA programming complete!");
    Ok(())
}

fn export_gpio(pin: u32) -> io::Result<()> {
    let path = format!("/sys/class/gpio/gpio{pin}");
    if !Path::new(&path).exists() {
        if let Ok(mut f) = OpenOptions::new()
            .write(true)
            .open("/sys/class/gpio/export")
        {
            let _ = f.write_all(pin.to_string().as_bytes());
        }
    }
    Ok(())
}

fn set_gpio_direction(pin: u32, dir: &str) -> io::Result<()> {
    let path = format!("/sys/class/gpio/gpio{pin}/direction");
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(dir.as_bytes())
}
