//! HM2004-DU FMCS access and UX3326 hardware operations.
use super::calibration;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::thread;
use std::time::Duration;

use crate::model::MeasurementEvidence;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

const FMCS_MCI_DEV: &str = "/dev/fmcs_mci";

// IOCTL Definitions for Linux AArch64 (Magic: 0xA5)
// _IOW(0xA5, 0, struct RegOp)  -> 0x4008A500
const IOC_WRITE_FPGA_REG: u64 = 0x4008A500;
// _IOWR(0xA5, 1, struct RegOp) -> 0xC008A501
const IOC_READ_FPGA_REG: u64 = 0xC008A501;
// _IOW(0xA5, 2, struct RegOp)  -> 0x4008A502
const IOC_WRITE_BOSA_REG: u64 = 0x4008A502;
// _IOWR(0xA5, 3, struct RegOp) -> 0xC008A503
const IOC_READ_BOSA_REG: u64 = 0xC008A503;

// BOSA controller (Semtech UX3326) sub-address base in FPGA register space
const BOSA_REG_BASE: u32 = 0x06006300;

// The discovery interval uses the configured default parameter 20.
// A microsecond contains 19440/125 upstream byte periods, rounded up.
const STOCK_DISCOVERY_PARAMETER: u32 = 20;
// MiniOltWriteFpgaReg takes table indices, unlike ForCmd and our raw ioctl.
// The timing and serial-grant table entries map to the FPGA register addresses below.
const FPGA_PON_MAC_TIMING: u32 = 0x01000000;
pub const FPGA_SERIAL_NUMBER_GRANT: u32 = 0x04000000;
pub const SERIAL_NUMBER_GRANT: u32 = ((STOCK_DISCOVERY_PARAMETER * 10 + 50) * 19440 + 124) / 125;

pub fn configure_discovery_timing() -> io::Result<()> {
    // Encode whole 125us frames in bits 16+ and byte periods for the remainder
    // in bits 0..15. This global MAC timing is not a measured, per-ONU
    // ranging/equalization delay.
    let us = STOCK_DISCOVERY_PARAMETER * 10 + 36;
    let value = ((us / 125) << 16) | (((us % 125) * 19440 + 124) / 125);
    write_fpga_reg(FPGA_PON_MAC_TIMING, value)?;
    println!("[+] Stock discovery timing: FPGA 0x{FPGA_PON_MAC_TIMING:08X}=0x{value:08X}, serial grant=0x{SERIAL_NUMBER_GRANT:05X}");
    Ok(())
}

// Full 288-byte verified calibration table from stock firmware (/h3c/bosa/modified_UX3326.bin).
pub const DEFAULT_UX3326_CALIBRATION_DATA: [u8; 288] = [
    0x02, 0x0e, 0x77, 0xc8, 0x14, 0x00, 0x3c, 0x02, 0x61, 0x00, 0x06, 0x22, 0x22, 0x1f, 0x40, 0x00,
    0x00, 0x00, 0x00, 0xab, 0xa8, 0x88, 0x40, 0xae, 0x00, 0xf8, 0x86, 0x85, 0x78, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x36, 0x0f, 0x00, 0x00, 0x00, 0x55, 0x00, 0x18, 0x00, 0x04, 0x35, 0x85, 0x4b, 0xc0,
    0x88, 0x80, 0x11, 0x80, 0x24, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x1b, 0xe6, 0x10, 0x00, 0x9f, 0x34, 0x03, 0xb6, 0x04, 0x00, 0x00, 0x00, 0x05, 0x7b, 0x04, 0x00,
    0x01, 0x51, 0x00, 0xe3, 0x01, 0x00, 0x00, 0x01, 0x1c, 0xcc, 0x10, 0x00, 0x97, 0x91, 0x00, 0x23,
    0x01, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7,
    0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7,
    0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe7, 0x00, 0xe8, 0x00, 0xe8, 0x00, 0xe9, 0x00, 0xe9,
    0x00, 0xe9, 0x00, 0xea, 0x00, 0xeb, 0x00, 0xec, 0x00, 0xec, 0x00, 0xec, 0x00, 0xec, 0x00, 0xed,
    0x00, 0xed, 0x00, 0xef, 0x00, 0xef, 0x00, 0xef, 0x00, 0xf0, 0x00, 0xf2, 0x00, 0xf2, 0x00, 0xf3,
    0x00, 0xf5, 0x00, 0xf8, 0x00, 0xfb, 0x00, 0xfc, 0x00, 0xfe, 0x01, 0x01, 0x01, 0x04, 0x01, 0x07,
    0x01, 0x08, 0x01, 0x0a, 0x01, 0x0d, 0x01, 0x0f, 0x01, 0x12, 0x01, 0x14, 0x01, 0x15, 0x01, 0x18,
    0x01, 0x1a, 0x01, 0x1d, 0x01, 0x1e, 0x01, 0x1f, 0x01, 0x21, 0x01, 0x24, 0x01, 0x26, 0x01, 0x28,
    0x01, 0x28, 0x01, 0x2b, 0x01, 0x2d, 0x01, 0x30, 0x01, 0x32, 0x01, 0x33, 0x01, 0x35, 0x01, 0x38,
    0x01, 0x3a, 0x01, 0x3b, 0x01, 0x3c, 0x01, 0x3f, 0x01, 0x41, 0x01, 0x44, 0x01, 0x46, 0x01, 0x47,
    0x01, 0x49, 0x01, 0x4a, 0x01, 0x4b, 0x01, 0x4d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

// Default 56-byte calibration table for Semtech UX3326 BOSA optical transceiver initialization.
pub const DEFAULT_UX3326_CALIBRATION_TABLE: [u8; 56] = [
    0x02, 0x0e, 0x77, 0xc8, 0x14, 0x00, 0x3c, 0x02, 0x61, 0x00, 0x06, 0x22, 0x22, 0x1f, 0x40, 0x00,
    0x00, 0x00, 0x00, 0xab, 0xa8, 0x88, 0x40, 0xae, 0x00, 0xf8, 0x86, 0x85, 0x78, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x36, 0x0f, 0x00, 0x00, 0x00, 0x55, 0x00, 0x18, 0x00, 0x04, 0x35, 0x85, 0x4b, 0xc0,
    0x88, 0x80, 0x11, 0x80, 0x24, 0x80, 0x00, 0x00,
];

#[repr(C)]
struct RegOp {
    addr: u32,
    val: u32,
}

#[cfg(unix)]
extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

#[cfg(unix)]
fn mci_file() -> io::Result<&'static std::fs::File> {
    static MCI_DEV: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    if let Some(f) = MCI_DEV.get() {
        return Ok(f);
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(FMCS_MCI_DEV)?;
    let _ = MCI_DEV.set(f);
    Ok(MCI_DEV.get().unwrap())
}

pub fn write_fpga_reg(addr: u32, val: u32) -> io::Result<()> {
    let _guard = if addr == FPGA_GPON_CONTROL {
        Some(
            GPON_CONTROL_LOCK
                .lock()
                .map_err(|_| io::Error::other("GPON control lock poisoned"))?,
        )
    } else {
        None
    };
    write_fpga_reg_unlocked(addr, val)
}

fn write_fpga_reg_unlocked(addr: u32, val: u32) -> io::Result<()> {
    let _diagnostic_epoch = super::diagnostics::hardware_write(addr);
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let mut op = RegOp { addr, val };
        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_WRITE_FPGA_REG, &mut op as *mut _) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (addr, val);
        Ok(())
    }
}

pub const FPGA_GPON_CONTROL: u32 = 0x01000004;
const GPON_TX: u32 = 0x02;
const GPON_FEC: u32 = 0x08;
static GPON_CONTROL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Board-specific GPON control bits. Serializes in-process RMW with raw writes.
/// External raw ioctl writers must not run concurrently with the daemon.
pub fn set_gpon_downstream(tx: bool, fec: bool) -> io::Result<u32> {
    let _guard = GPON_CONTROL_LOCK
        .lock()
        .map_err(|_| io::Error::other("GPON control lock poisoned"))?;
    update_gpon_control(
        tx,
        fec,
        || read_fpga_reg(FPGA_GPON_CONTROL),
        |value| write_fpga_reg_unlocked(FPGA_GPON_CONTROL, value),
    )
}

fn update_gpon_control(
    tx: bool,
    fec: bool,
    mut read: impl FnMut() -> io::Result<u32>,
    mut write: impl FnMut(u32) -> io::Result<()>,
) -> io::Result<u32> {
    let before = read()?;
    let target = (before & !(GPON_TX | GPON_FEC))
        | if tx { GPON_TX } else { 0 }
        | if fec { GPON_FEC } else { 0 };
    if before != target {
        write(target)?;
    }
    let actual = read()?;
    if actual != target {
        return Err(io::Error::other(format!(
            "GPON control readback mismatch: wanted {target:#010x}, got {actual:#010x}"
        )));
    }
    Ok(actual)
}

/// Explicit driver mapping after a verified Alloc-ID channel transition.
/// Returns the previous mapping (zero means the initial ONU-ID channel).
pub fn exchange_omci_channel(id: u8, channel: u16) -> io::Result<u16> {
    if id > 15 || (channel != 0 && channel != 0x100 + id as u16) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "OMCI channel"));
    }
    #[cfg(unix)]
    {
        let file = mci_file()?;
        let mut op = RegOp {
            addr: id as u32,
            val: channel as u32,
        };
        if unsafe { ioctl(file.as_raw_fd(), 0xC008A509u64, &mut op as *mut _) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(op.val as u16)
    }
    #[cfg(not(unix))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux FMCS required",
        ))
    }
}

pub fn read_fpga_reg(addr: u32) -> io::Result<u32> {
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let mut op = RegOp { addr, val: 0 };
        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_READ_FPGA_REG, &mut op as *mut _) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(op.val)
    }
    #[cfg(not(unix))]
    {
        let _ = addr;
        Ok(0)
    }
}

pub fn write_bosa_reg(reg: u32, val: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let addr = if reg < 0x10000 {
            BOSA_REG_BASE | reg
        } else {
            reg
        };
        let mut op = RegOp { addr, val };
        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_WRITE_BOSA_REG, &mut op as *mut _) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (reg, val);
        Ok(())
    }
}

pub fn read_bosa_reg(reg: u32) -> io::Result<u32> {
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let addr = if reg < 0x10000 {
            BOSA_REG_BASE | reg
        } else {
            reg
        };
        let mut op = RegOp { addr, val: 0 };
        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_READ_BOSA_REG, &mut op as *mut _) };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(op.val)
    }
    #[cfg(not(unix))]
    {
        let _ = reg;
        Ok(0)
    }
}

/// Parsed calibration parameters from modified_UX3326.bin / default_UX3326.bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BosaCalibration {
    pub temp_primary: (u16, u16, i16), // N (0x40), D (0x42), O (0x44)
    pub voltage: (u16, u16, i16),      // N (0x46), D (0x48), O (0x4a)
    pub bias: (u16, u16, i16),         // N (0x4c), D (0x4e), O (0x50)
    pub tx_power: (u16, u16, i16),     // N (0x52), D (0x54), O (0x56)
    pub temp_alt: (u16, u16, i16),     // N (0x58), D (0x5a), O (0x5c)
    pub temp_threshold: u16,           // unsigned BE16 threshold (0x5e), per stock
}

impl BosaCalibration {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 0x60 {
            return None;
        }
        let read_u16 = |off: usize| -> u16 { u16::from_be_bytes([bytes[off], bytes[off + 1]]) };
        let read_i16 = |off: usize| -> i16 { i16::from_be_bytes([bytes[off], bytes[off + 1]]) };

        Some(Self {
            temp_primary: (read_u16(0x40), read_u16(0x42), read_i16(0x44)),
            voltage: (read_u16(0x46), read_u16(0x48), read_i16(0x4a)),
            bias: (read_u16(0x4c), read_u16(0x4e), read_i16(0x50)),
            tx_power: (read_u16(0x52), read_u16(0x54), read_i16(0x56)),
            temp_alt: (read_u16(0x58), read_u16(0x5a), read_i16(0x5c)),
            temp_threshold: read_u16(0x5e),
        })
    }
}

pub const FACTORY_UX3326_OFFSET: u64 = 0x2200;
pub const FACTORY_UX3326_LEN: usize = 288;

/// Tries to locate and read 288 bytes from the 1MB factory partition (UBI volume or MTD) at offset 0x2200.
pub fn read_factory_calibration() -> Option<[u8; 288]> {
    factory_calibration().1
}

pub fn factory_calibration() -> (Option<String>, Option<[u8; 288]>) {
    let mut detected = None;
    #[cfg(unix)]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut candidate_devices = Vec::new();

        // 1. Scan UBI sysfs for volume named "factory"
        if let Ok(entries) = fs::read_dir("/sys/class/ubi") {
            for entry in entries.flatten() {
                let name_path = entry.path().join("name");
                if let Ok(name) = fs::read_to_string(&name_path) {
                    if name.trim() == "factory" {
                        if let Some(file_name) = entry.file_name().to_str() {
                            candidate_devices.push(format!("/dev/{file_name}"));
                        }
                    }
                }
            }
        }

        // 2. Direct device nodes
        candidate_devices.push("/dev/ubi0_factory".to_string());

        // 3. Scan /proc/mtd for "factory" partition
        if let Ok(mtd_info) = fs::read_to_string("/proc/mtd") {
            for line in mtd_info.lines() {
                if line.contains("\"factory\"") {
                    if let Some(dev_part) = line.split(':').next() {
                        let dev_str = dev_part.trim();
                        candidate_devices.push(format!("/dev/{dev_str}ro"));
                        candidate_devices.push(format!("/dev/{dev_str}"));
                        let mtd_num = dev_str.trim_start_matches("mtd");
                        candidate_devices.push(format!("/dev/mtdblock{mtd_num}"));
                    }
                }
            }
        }

        for dev in candidate_devices {
            if let Ok(mut file) = fs::File::open(&dev) {
                if detected.is_none() {
                    detected = Some(dev.clone());
                }
                if file.seek(SeekFrom::Start(FACTORY_UX3326_OFFSET)).is_ok() {
                    let mut buf = [0u8; FACTORY_UX3326_LEN];
                    if file.read_exact(&mut buf).is_ok() {
                        // Check if buffer is non-blank (not all 0xFF and not all 0x00)
                        if !buf.iter().all(|&b| b == 0xFF) && !buf.iter().all(|&b| b == 0x00) {
                            return (Some(dev), Some(buf));
                        }
                    }
                }
            }
        }
    }
    (detected, None)
}

/// Read 16-bit ADC register pair (high byte at base_reg, low byte at base_reg + 1).
/// UX3326 requires 1ms wait after each byte read.
pub fn read_adc_u16(base_reg: u32) -> io::Result<u16> {
    #[cfg(unix)]
    {
        let hi = read_bosa_reg(base_reg)?;
        thread::sleep(Duration::from_millis(1));
        let lo = read_bosa_reg(base_reg + 1)?;
        thread::sleep(Duration::from_millis(1));
        Ok(((hi as u16 & 0xff) << 8) | (lo as u16 & 0xff))
    }
    #[cfg(not(unix))]
    {
        let _ = base_reg;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux FMCS required",
        ))
    }
}

fn scaled_adc(raw: u16, (n, d, o): (u16, u16, i16)) -> Option<f32> {
    let product = i32::try_from(u32::from(raw) * u32::from(n)).ok()?;
    if d == 0 {
        return None;
    }
    Some(product as f32 / d as f32 + o as f32)
}

pub fn calc_temperature(raw: u16, cal: &BosaCalibration) -> f32 {
    let temp = scaled_adc(raw, cal.temp_primary).unwrap_or(f32::NAN) / 256.0;
    if temp <= cal.temp_threshold as f32 {
        scaled_adc(raw, cal.temp_alt).unwrap_or(f32::NAN) / 256.0
    } else {
        temp
    }
}

pub fn calc_voltage(raw: u16, cal: &BosaCalibration) -> f32 {
    scaled_adc(raw, cal.voltage).unwrap_or(f32::NAN) / 10000.0
}

pub fn calc_bias(raw: u16, cal: &BosaCalibration) -> f32 {
    scaled_adc(raw, cal.bias).unwrap_or(f32::NAN) / 500.0
}

pub fn calc_tx_power(raw: u16, cal: &BosaCalibration) -> Option<f32> {
    let p_mw = (scaled_adc(raw, cal.tx_power)? / 10.0) / 1000.0;
    if p_mw > 0.0 {
        Some((10.0 * f64::from(p_mw).log10()) as f32)
    } else {
        None
    }
}

pub fn init_optical_transceiver() -> io::Result<()> {
    set_optical_tx_enable(true)
}

fn init_optical_transceiver_with(snapshot: &calibration::Snapshot) -> io::Result<()> {
    println!("[*] === Initializing Micro-OLT BOSA Transceiver (UX3326) ===");

    // Step 1: Set FPGA optical transceiver clock sync mode.
    println!("[*] Step 1: Configuring FPGA CDR clock sync (0x00000020 = 0x17258)...");
    write_fpga_reg(0x00000020, 0x17258)?;

    // Step 2: Optical hardware reset and ready verification.
    println!("[*] Step 2: Performing hardware reset via FPGA register 0x84...");
    write_fpga_reg(0x84, 0)?;
    thread::sleep(Duration::from_millis(250));
    write_fpga_reg(0x84, 1)?;
    thread::sleep(Duration::from_millis(250));

    let reset_status = read_fpga_reg(0x84)?;
    println!("[+] FPGA reset line state: 0x{:08X}", reset_status);
    if reset_status != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FPGA reset readback differs from 1; SPI alignment must be corrected",
        ));
    }

    // Readiness is determined by transport success, not register 0x27's value.
    let mut ready = false;
    for attempt in 1..=10 {
        if read_bosa_reg(0x27).is_ok() {
            println!(
                "[+] UX3326 register access succeeded on attempt {}",
                attempt
            );
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    if !ready {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "UX3326 register access failed",
        ));
    }

    // Step 3: Load calibration table
    let cal_table = &snapshot.bytes[..56];

    // Step 4: Program calibration table into BOSA registers.
    println!("[*] Step 3: Enabling BOSA bus gate on FPGA (0x50 = 1)...");
    write_fpga_reg(0x50, 1)?;
    thread::sleep(Duration::from_micros(500));

    println!("[*] Programming 56 calibration registers (skipping 0x27 latch register)...");
    let program_result = (|| -> io::Result<()> {
        for reg in 0..=0x37u32 {
            if reg == 0x27 {
                continue; // Must skip register 0x27 during sequential write
            }
            let val = cal_table[reg as usize] as u32;
            write_bosa_reg(reg, val)?;
            thread::sleep(Duration::from_millis(1));
        }

        // Write latch/save command (0x55) to register 0x27 to commit calibration
        println!("[*] Committing calibration latch (register 0x27 = 0x55)...");
        write_bosa_reg(0x27, 0x55)?;
        thread::sleep(Duration::from_millis(1));
        Ok(())
    })();

    // Disable BOSA bus gate on FPGA
    println!("[*] Disabling BOSA bus gate on FPGA (0x50 = 0)...");
    let gate_result = write_fpga_reg(0x50, 0);
    program_result?;
    gate_result?;
    thread::sleep(Duration::from_millis(1));

    if read_bosa_reg(6)? as u8 != cal_table[6] || read_bosa_reg(7)? as u8 != cal_table[7] {
        return Err(io::Error::other("initial BOSA code pair readback mismatch"));
    }

    println!("[+] Micro-OLT BOSA optical transceiver successfully initialized!");
    Ok(())
}

pub use crate::model::OpticalTransceiverStatus;

use std::sync::Mutex;
use std::time::Instant;

static OPTICAL_CACHE: Mutex<Option<(Instant, OpticalTransceiverStatus)>> = Mutex::new(None);

pub fn invalidate_optical_cache() {
    if let Ok(mut guard) = OPTICAL_CACHE.lock() {
        *guard = None;
    }
}

pub const FPGA_REG_OPTICAL_RESET: u32 = 0x84;

pub(crate) fn parse_optical_reset_state(val: u32) -> io::Result<bool> {
    match val {
        1 => Ok(true),
        0 => Ok(false),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unexpected FPGA reset line state 0x{other:08X} (expected 0 or 1)"),
        )),
    }
}

/// Controls downstream PON optical transmission via hardware reset line (FPGA register 0x84).
///
/// When `enable` is false (OFF):
/// - Writes 0 to FPGA register 0x84, asserting RESET_N (hardware reset/power-down).
/// - Asserts the UX3326 reset line; this readback is not an optical power measurement.
/// - Verifies readback of register 0x84 is 0.
///
/// When `enable` is true (ON):
/// - UX3326 registers are volatile and lose calibration across reset.
/// - Performs the full optical initialization sequence via `init_optical_transceiver()`,
///   releasing reset (0x84 = 1), waiting for stabilization, and reprogramming the 56-byte
///   selected calibration table followed by latching.
/// - Verifies readback of register 0x84 is 1.
pub fn set_optical_tx_enable(enable: bool) -> io::Result<()> {
    // Resolve before any hardware side effect. The owner uses exactly this immutable
    // version for initialization, telemetry and the temperature table.
    let candidate = if enable {
        Some(calibration::resolve()?)
    } else {
        None
    };
    let mut runtime = calibration::RUNTIME.write().unwrap();
    // Cover stabilization delays and calibration programming between raw writes.
    let _diagnostic_epoch = super::diagnostics::hardware_write(FPGA_REG_OPTICAL_RESET);
    runtime.transition(candidate, |candidate| {
        if enable {
            init_optical_transceiver_with(candidate.unwrap())?;
            let verify = read_fpga_reg(FPGA_REG_OPTICAL_RESET)?;
            if verify != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "Optical TX enable verification failed: expected 0x00000001, got 0x{verify:08X}"
                    ),
                ));
            }
        } else {
            write_fpga_reg(FPGA_REG_OPTICAL_RESET, 0)?;
            thread::sleep(Duration::from_millis(10));
            let verify = read_fpga_reg(FPGA_REG_OPTICAL_RESET)?;
            if verify != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "Optical TX disable verification failed: expected 0x00000000, got 0x{verify:08X}"
                    ),
                ));
            }
        }
        Ok(())
    })
}

/// Reads the downstream PON optical transceiver reset line state via FPGA register 0x84.
///
/// Returns:
/// - `Ok(true)`: 0x84 is 1 (RESET_N released; not proof of calibration or emitted light)
/// - `Ok(false)`: 0x84 is 0 (hardware reset/power-down state)
pub fn get_optical_tx_enable() -> io::Result<bool> {
    let val = read_fpga_reg(FPGA_REG_OPTICAL_RESET)?;
    parse_optical_reset_state(val)
}

pub fn get_optical_status() -> OpticalTransceiverStatus {
    let runtime = calibration::RUNTIME.read().unwrap();
    if let Ok(guard) = OPTICAL_CACHE.lock() {
        if let Some((ts, ref status)) = *guard {
            if ts.elapsed() < Duration::from_secs(2) {
                return status.clone();
            }
        }
    }

    let cal = runtime.active.as_ref().map(|s| s.telemetry);
    let tx_enabled = get_optical_tx_enable().ok();

    let mut measurements = BTreeMap::new();

    // UX3326 internal ADC channels:
    // 0x2E/0x2F: Temperature
    // 0x30/0x31: Supply Voltage (Vcc)
    // 0x32/0x33: Laser Bias Current
    // 0x34/0x35: Transmit Power
    let unavailable = if tx_enabled == Some(false) {
        "optical_tx_disabled_hardware_reset"
    } else if tx_enabled.is_none() {
        "optical_reset_state_unknown"
    } else {
        "calibration_not_applied_by_owner"
    };
    let (temperature_celsius, vcc_voltage, laser_bias_current_ma, tx_power_dbm) = if tx_enabled
        != Some(true)
        || !runtime.hardware_applied
    {
        // When hardware reset is asserted (FPGA 0x84 = 0), UX3326 is powered down
        // and SPI reads return floating idle (0x4040). Do not interpret as valid telemetry.
        measurements.insert(
            "temperature_celsius".into(),
            MeasurementEvidence::unavailable("UX3326/FMCS", "C", unavailable),
        );
        measurements.insert(
            "vcc_voltage".into(),
            MeasurementEvidence::unavailable("UX3326/FMCS", "V", unavailable),
        );
        measurements.insert(
            "laser_bias_current_ma".into(),
            MeasurementEvidence::unavailable("UX3326/FMCS", "mA", unavailable),
        );
        measurements.insert(
            "tx_power_dbm".into(),
            MeasurementEvidence::unavailable("UX3326/FMCS", "dBm", unavailable),
        );
        (None, None, None, None)
    } else {
        let cal = cal.expect("applied runtime has a calibration snapshot");
        let temp_res = read_adc_u16(0x2e);
        let volt_res = read_adc_u16(0x30);
        let bias_res = read_adc_u16(0x32);
        let tx_res = read_adc_u16(0x34);

        let temp = match temp_res {
            Ok(raw) => {
                let val = calc_temperature(raw, &cal);
                measurements.insert(
                    "temperature_celsius".into(),
                    MeasurementEvidence::observed(
                        "UX3326 ADC Reg 0x2E/0x2F with modified_UX3326.bin calibration",
                        "C",
                        format!("{raw:04x}"),
                    ),
                );
                if val.is_finite() {
                    Some(val)
                } else {
                    measurements.insert(
                        "temperature_celsius".into(),
                        MeasurementEvidence::unavailable(
                            "UX3326/FMCS",
                            "C",
                            "invalid_calibration_or_adc_overflow",
                        ),
                    );
                    None
                }
            }
            Err(_) => {
                measurements.insert(
                    "temperature_celsius".into(),
                    MeasurementEvidence::unavailable("UX3326/FMCS", "C", "bosa_adc_read_failed"),
                );
                None
            }
        };

        let volt = match volt_res {
            Ok(raw) => {
                let val = calc_voltage(raw, &cal);
                measurements.insert(
                    "vcc_voltage".into(),
                    MeasurementEvidence::observed(
                        "UX3326 ADC Reg 0x30/0x31 with modified_UX3326.bin calibration",
                        "V",
                        format!("{raw:04x}"),
                    ),
                );
                if val.is_finite() {
                    Some(val)
                } else {
                    measurements.insert(
                        "vcc_voltage".into(),
                        MeasurementEvidence::unavailable(
                            "UX3326/FMCS",
                            "V",
                            "invalid_calibration_or_adc_overflow",
                        ),
                    );
                    None
                }
            }
            Err(_) => {
                measurements.insert(
                    "vcc_voltage".into(),
                    MeasurementEvidence::unavailable("UX3326/FMCS", "V", "bosa_adc_read_failed"),
                );
                None
            }
        };

        let bias = match bias_res {
            Ok(raw) => {
                let val = calc_bias(raw, &cal);
                measurements.insert(
                    "laser_bias_current_ma".into(),
                    MeasurementEvidence::observed(
                        "UX3326 ADC Reg 0x32/0x33 with modified_UX3326.bin calibration",
                        "mA",
                        format!("{raw:04x}"),
                    ),
                );
                if val.is_finite() {
                    Some(val)
                } else {
                    measurements.insert(
                        "laser_bias_current_ma".into(),
                        MeasurementEvidence::unavailable(
                            "UX3326/FMCS",
                            "mA",
                            "invalid_calibration_or_adc_overflow",
                        ),
                    );
                    None
                }
            }
            Err(_) => {
                measurements.insert(
                    "laser_bias_current_ma".into(),
                    MeasurementEvidence::unavailable("UX3326/FMCS", "mA", "bosa_adc_read_failed"),
                );
                None
            }
        };

        let tx = match tx_res {
            Ok(raw) => {
                let val = calc_tx_power(raw, &cal);
                if let Some(v) = val {
                    measurements.insert(
                        "tx_power_dbm".into(),
                        MeasurementEvidence::observed(
                            "UX3326 ADC Reg 0x34/0x35 with modified_UX3326.bin calibration",
                            "dBm",
                            format!("{raw:04x}"),
                        ),
                    );
                    Some(v)
                } else {
                    measurements.insert(
                        "tx_power_dbm".into(),
                        MeasurementEvidence::unavailable(
                            "UX3326/FMCS",
                            "dBm",
                            "tx_power_non_positive_or_invalid_calibration",
                        ),
                    );
                    None
                }
            }
            Err(_) => {
                measurements.insert(
                    "tx_power_dbm".into(),
                    MeasurementEvidence::unavailable("UX3326/FMCS", "dBm", "bosa_adc_read_failed"),
                );
                None
            }
        };

        (temp, volt, bias, tx)
    };

    if let Some(enabled) = tx_enabled {
        measurements.insert(
            "tx_enabled".into(),
            MeasurementEvidence::observed(
                "FPGA Reg 0x84 (UX3326 RESET_N)",
                "boolean",
                if enabled {
                    "true (reset released; not a measured light state)"
                } else {
                    "false (reset/powered_down)"
                },
            ),
        );
    }

    let status = OpticalTransceiverStatus {
        model: "UX3326".to_string(),
        wavelength_tx_nm: 1490,
        wavelength_rx_nm: 1310,
        phy_rate_downlink_gbps: 2.488,
        phy_rate_uplink_gbps: 1.244,
        tx_power_dbm,
        laser_bias_current_ma,
        temperature_celsius,
        vcc_voltage,
        tx_enabled,
        measurements,
        calibration_sha256: runtime
            .active
            .as_ref()
            .map(|s| s.version.effective_sha256.clone()),
        calibration_applied: runtime.hardware_applied,
    };

    if let Ok(mut guard) = OPTICAL_CACHE.lock() {
        *guard = Some((Instant::now(), status.clone()));
    }

    status
}

#[repr(C)]
pub struct PloamMsg {
    pub data: [u8; 16],
    pub len: u32,
}

const IOC_RECV_PLOAM: u64 = 0x8014A505;
const IOC_SEND_PACKET: u64 = 0x4014A506;

pub fn send_ploam_msg(data: &[u8]) -> io::Result<()> {
    if data.len() != 13 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PLOAM requires 13 bytes",
        ));
    }
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let mut msg = PloamMsg {
            data: [0u8; 16],
            len: data.len() as u32,
        };
        let copy_len = data.len().min(16);
        msg.data[..copy_len].copy_from_slice(&data[..copy_len]);

        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_SEND_PACKET, &msg as *const _) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        remember_downstream(data);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = data;
        Ok(())
    }
}

/// Downstream PLOAM frames we transmitted recently.
///
/// The micro-OLT's own receiver observes the downstream stream it just sent:
/// `Upstream_Overhead` is a broadcast (ONU-ID 0xFF) message with ID 0x01, which
/// is bit-for-bit the shape of an upstream `Serial_Number_ONU` report. Without
/// this registry such an echo is indistinguishable from a real ONU announcing
/// itself, so it reserves a slot and shows up as a permanently offline
/// "Unknown" device. Matching the exact bytes we sent separates the two
/// without guessing at vendor codes or serial ranges.
static DOWNSTREAM_ECHOES: std::sync::Mutex<Vec<([u8; 13], std::time::Instant)>> =
    std::sync::Mutex::new(Vec::new());

/// How long a transmitted frame stays eligible as an echo candidate.
const ECHO_WINDOW: Duration = Duration::from_secs(3);
/// Bounded so a stuck entry can never grow the set without limit.
const ECHO_LIMIT: usize = 64;

fn remember_downstream(frame: &[u8]) {
    let mut echo = [0u8; 13];
    echo.copy_from_slice(&frame[..13]);
    let Ok(mut echoes) = DOWNSTREAM_ECHOES.lock() else {
        return;
    };
    echoes.retain(|(_, at)| at.elapsed() < ECHO_WINDOW);
    if echoes.len() >= ECHO_LIMIT {
        echoes.remove(0);
    }
    echoes.push((echo, std::time::Instant::now()));
}

/// True when an observed "upstream" frame is byte-identical to something this
/// OLT sent within the echo window.
///
/// ONU-ID, message ID and the 10 payload bytes are compared; the CRC byte is
/// excluded because it is derived from the same fields and some FPGA
/// revisions re-emit it. A genuine ONU report cannot collide with these
/// values, since it carries the ONU's own vendor code and serial number.
pub fn is_downstream_echo(observed: &[u8]) -> bool {
    if observed.len() < 12 {
        return false;
    }
    let Ok(mut echoes) = DOWNSTREAM_ECHOES.lock() else {
        return false;
    };
    echoes.retain(|(_, at)| at.elapsed() < ECHO_WINDOW);
    let matched = echoes
        .iter()
        .any(|(frame, _)| frame[..12] == observed[..12]);
    if matched {
        // One echo per transmission: do not let a repeated reflection keep
        // matching after it has already been accounted for.
        echoes.retain(|(frame, _)| frame[..12] != observed[..12]);
    }
    matched
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RawPloamMsg {
    data: [u8; 16],
    len: usize,
}

impl RawPloamMsg {
    pub fn new(data: [u8; 16], len: usize) -> io::Result<Self> {
        if len > 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid PLOAM ioctl length",
            ));
        }
        Ok(Self { data, len })
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::ops::Deref for RawPloamMsg {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for RawPloamMsg {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::borrow::Borrow<[u8]> for RawPloamMsg {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::fmt::Debug for RawPloamMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_slice(), f)
    }
}

pub fn recv_ploam_msg() -> io::Result<RawPloamMsg> {
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let mut msg = PloamMsg {
            data: [0u8; 16],
            len: 0,
        };

        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_RECV_PLOAM, &mut msg as *mut _) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        RawPloamMsg::new(msg.data, msg.len as usize)
    }
    #[cfg(not(unix))]
    {
        Ok(RawPloamMsg {
            data: [0u8; 16],
            len: 0,
        })
    }
}

#[repr(C)]
pub struct CarrierReq {
    pub port: u32,
    pub carrier: u32,
}

const IOC_SET_CARRIER: u64 = 0x4008A507;

/// Set carrier for a protocol ONU-ID, translating to the driver's 1-based port ABI.
pub fn set_fttr_carrier(onu_id: u32, carrier: bool) -> io::Result<()> {
    if onu_id >= super::RESOURCES.onu_ids.len() as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU ID must be 0..15",
        ));
    }
    let port = onu_id + 1;
    #[cfg(unix)]
    {
        let file = mci_file()?;

        let mut req = CarrierReq {
            port,
            carrier: if carrier { 1 } else { 0 },
        };

        let ret = unsafe { ioctl(file.as_raw_fd(), IOC_SET_CARRIER, &mut req as *mut _) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (port, carrier);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gpon_control_bits_are_independent_and_preserve_other_bits() {
        use std::cell::Cell;
        for tx in [false, true] {
            for fec in [false, true] {
                for before in [0, 2, 8, 10, 0xffff_ffff, 0xa5a5_5555] {
                    let value = Cell::new(before);
                    let actual = super::update_gpon_control(
                        tx,
                        fec,
                        || Ok(value.get()),
                        |v| {
                            value.set(v);
                            Ok(())
                        },
                    )
                    .unwrap();
                    assert_eq!(actual & !10, before & !10);
                    assert_eq!(actual & 2 != 0, tx);
                    assert_eq!(actual & 8 != 0, fec);
                }
            }
        }
    }

    #[test]
    fn gpon_control_rejects_readback_mismatch_and_transport_errors() {
        assert!(super::update_gpon_control(true, true, || Ok(0), |_| Ok(())).is_err());
        assert!(super::update_gpon_control(
            true,
            true,
            || Err(std::io::Error::other("read failed")),
            |_| panic!("must not write without baseline")
        )
        .is_err());
        assert!(super::update_gpon_control(
            true,
            true,
            || Ok(0),
            |_| Err(std::io::Error::other("write failed"))
        )
        .is_err());
        assert_eq!(
            super::update_gpon_control(
                true,
                true,
                || Ok(10),
                |_| panic!("unchanged state must not write")
            )
            .unwrap(),
            10
        );
    }

    use super::*;

    #[test]
    fn test_raw_ploam_msg_exact_length_and_deref() {
        let mut data = [0u8; 16];
        data[0..13].copy_from_slice(&[4, 9, 10, 4, 10, 16, 64, 1, 0, 0, 0, 0, 151]);
        data[13..16].copy_from_slice(&[0xff, 0xff, 0xff]); // trailing junk that must not leak
        let msg = RawPloamMsg::new(data, 13).expect("valid 13-byte PLOAM length");
        assert_eq!(msg.len(), 13);
        assert!(!msg.is_empty());
        assert_eq!(msg.as_slice().len(), 13);
        assert_eq!(&msg[..], &data[..13]);
        assert_eq!(msg[0], 4);
        assert_eq!(msg[1], 9);
        assert_eq!(msg[12], 151);
        assert_eq!(format!("{:02X?}", msg), format!("{:02X?}", &data[..13]));
    }

    #[test]
    fn test_raw_ploam_msg_empty() {
        let msg = RawPloamMsg::new([0; 16], 0).expect("empty PLOAM length 0 is valid");
        assert_eq!(msg.len(), 0);
        assert!(msg.is_empty());
        assert_eq!(msg.as_slice(), &[] as &[u8]);
    }

    #[test]
    fn test_raw_ploam_msg_overflow_rejected() {
        let err = RawPloamMsg::new([0; 16], 17).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "invalid PLOAM ioctl length");

        let err2 = RawPloamMsg::new([0; 16], 255).unwrap_err();
        assert_eq!(err2.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_calibration_data_matches_stock_table() {
        assert_eq!(
            &DEFAULT_UX3326_CALIBRATION_DATA[..56],
            &DEFAULT_UX3326_CALIBRATION_TABLE[..],
            "initialization registers must match stock table"
        );
    }

    #[test]
    fn test_bosa_calibration_from_bytes() {
        let cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA)
            .expect("calibration table parsing failed");
        assert_eq!(cal.temp_primary, (7142, 4096, -24780));
        assert_eq!(cal.voltage, (950, 1024, 0));
        assert_eq!(cal.bias, (1403, 1024, 337));
        assert_eq!(cal.tx_power, (227, 256, 1));
        assert_eq!(cal.temp_alt, (7372, 4096, -26735));
        assert_eq!(cal.temp_threshold, 35);
    }

    #[test]
    fn test_calc_temperature() {
        let cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA).unwrap();
        // Live hardware sample: 0x5980 = 22912 -> ~59.26 C
        let temp = calc_temperature(0x5980, &cal);
        assert!((temp - 59.26).abs() < 0.1, "expected ~59.26 C, got {temp}");

        // Low temperature branch: raw = 0x3000 = 12288
        // Primary: (12288 * 7142 / 4096 - 24780) / 256 = (21426 - 24780) / 256 = -13.1 C <= 35
        // Alt: (12288 * 7372 / 4096 - 26735) / 256 = (22116 - 26735) / 256 = -18.04 C
        let temp_low = calc_temperature(0x3000, &cal);
        assert!(temp_low <= 35.0);
        assert!((temp_low - (-18.04)).abs() < 0.2);
    }

    #[test]
    fn test_calc_voltage() {
        let cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA).unwrap();
        // Live hardware sample: 0x8880 = 34944 -> ~3.24 V
        let volt = calc_voltage(0x8880, &cal);
        assert!((volt - 3.24).abs() < 0.05, "expected ~3.24 V, got {volt}");
    }

    #[test]
    fn test_calc_bias() {
        let cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA).unwrap();
        // Live hardware sample: 0x1F00 = 7936 -> ~22.42 mA
        let bias = calc_bias(0x1F00, &cal);
        assert!((bias - 22.42).abs() < 0.1, "expected ~22.42 mA, got {bias}");
    }

    #[test]
    fn test_calc_tx_power() {
        let cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA).unwrap();
        // Live hardware sample: 0x2440 = 9280
        let p = calc_tx_power(0x2440, &cal);
        assert!(p.is_some());

        // Zero raw -> non-positive or very low
        let zero = calc_tx_power(0, &cal);
        assert!(zero.is_some());
    }

    #[test]
    fn telemetry_preserves_fractional_quotients_and_rejects_invalid_coefficients() {
        let mut cal = BosaCalibration::from_bytes(&DEFAULT_UX3326_CALIBRATION_DATA).unwrap();
        assert_eq!(calc_temperature(0x5980, &cal), 59.26001);
        assert_eq!(calc_voltage(0x8881, &cal), 3.2419677);
        assert_eq!(calc_bias(0x1f01, &cal), 22.42324);
        assert!((calc_tx_power(0x2441, &cal).unwrap() - -0.8456657).abs() < 0.000001);
        cal.voltage.1 = 0;
        assert!(!calc_voltage(1, &cal).is_finite());
        cal.tx_power.1 = 0;
        assert!(calc_tx_power(0x2441, &cal).is_none());
        cal.temp_primary = (u16::MAX, 1, 0);
        assert!(!calc_temperature(u16::MAX, &cal).is_finite());
        let mut bytes = DEFAULT_UX3326_CALIBRATION_DATA;
        bytes[0x5e..0x60].copy_from_slice(&u16::MAX.to_be_bytes());
        let cal = BosaCalibration::from_bytes(&bytes).unwrap();
        assert_eq!(cal.temp_threshold, u16::MAX);
        assert_eq!(
            calc_temperature(0x5980, &cal),
            scaled_adc(0x5980, cal.temp_alt).unwrap() / 256.0
        );
    }

    #[test]
    fn test_get_optical_status_fallback_evidence() {
        // Without hardware device, status gracefully reports model and evidences
        let status = get_optical_status();
        assert_eq!(status.model, "UX3326");
        assert_eq!(status.wavelength_tx_nm, 1490);
        assert_eq!(status.wavelength_rx_nm, 1310);
        assert!(status.measurements.contains_key("temperature_celsius"));
        assert!(status.measurements.contains_key("vcc_voltage"));
        assert!(status.measurements.contains_key("laser_bias_current_ma"));
    }

    #[test]
    fn test_optical_tx_reset_line_semantics() {
        assert_eq!(FPGA_REG_OPTICAL_RESET, 0x84);
        assert_eq!(parse_optical_reset_state(1).unwrap(), true);
        assert_eq!(parse_optical_reset_state(0).unwrap(), false);
        assert!(parse_optical_reset_state(2).is_err());
        assert!(parse_optical_reset_state(0xFFFFFFFF).is_err());
    }

    use crate::pon_engine::ploam::PloamFrame;

    fn clear_echoes() {
        DOWNSTREAM_ECHOES.lock().unwrap().clear();
    }

    #[test]
    fn downstream_upstream_overhead_is_recognised_as_an_echo() {
        clear_echoes();
        // The discovery frame that shares ID 0x01 with Serial_Number_ONU.
        let overhead = crate::platform::h3c_hm2004_du::activation::discovery_frames()[0].to_bytes();
        remember_downstream(&overhead);
        assert!(is_downstream_echo(&overhead));
        // A reflection is accounted for once; a second identical frame is a
        // fresh transmission candidate, not the same echo replayed.
        assert!(!is_downstream_echo(&overhead));
    }

    #[test]
    fn a_real_onu_serial_report_is_not_mistaken_for_an_echo() {
        clear_echoes();
        let overhead = crate::platform::h3c_hm2004_du::activation::discovery_frames()[0].to_bytes();
        remember_downstream(&overhead);
        // Upstream Serial_Number_ONU from a real ONT: ONU-ID 0xFF, ID 0x01,
        // but the payload carries the ONU's own vendor code and serial.
        let report = PloamFrame::new(
            0xff,
            1,
            [0x48, 0x57, 0x54, 0x43, 0x12, 0x34, 0x56, 0x78, 0, 0],
        )
        .to_bytes();
        assert!(!is_downstream_echo(&report));
    }

    #[test]
    fn echoes_expire_and_the_registry_stays_bounded() {
        clear_echoes();
        {
            let mut echoes = DOWNSTREAM_ECHOES.lock().unwrap();
            for n in 0..(ECHO_LIMIT + 10) {
                echoes.push(([0u8; 13], std::time::Instant::now() - ECHO_WINDOW * 2));
                let _ = n;
            }
            // All entries are stale, so nothing can match.
            assert!(echoes.len() > ECHO_LIMIT);
        }
        let stale = [0u8; 13];
        assert!(!is_downstream_echo(&stale));
        assert!(DOWNSTREAM_ECHOES.lock().unwrap().is_empty());
    }
}
