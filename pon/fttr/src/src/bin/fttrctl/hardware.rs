//! Offline diagnostics use the same platform identity and optical owner lock.
use openwrt_fttr::platform::diagnostics::{HardwareDiagnostics, RegisterSpace};
use std::{io, path::Path};

const DEFAULT_BOSA_CAL_SOURCE: &str = "/dev/ubi0_3";
const DEFAULT_BOSA_CAL_OFFSET: u64 = 0x1a_5000;

const USAGE: &str = "Usage: fttrctl hardware DEVICE COMMAND [ARGS]
  DEVICE is the platform register endpoint, e.g. /dev/fsl-1fbd4000.fsl.
  status
  init
  calibrate-bosa [SOURCE [OFFSET]]
      Verifies the factory table and programs the UX3326S over I2C (802 writes).
      Disables downstream TX first and leaves it off; requires fttrd to be
      stopped and the optical owner flock to be free.
  reg-read ADDRESS WORDS
  reg-write ADDRESS WORD...
  mem-read ADDRESS ROW WORDS
  mem-write ADDRESS ROW WORD...
  bosa-read ADDRESS
  bosa-write ADDRESS VALUE
  tx on|off
  fec on|off
Offline exclusive session; fails while fttrd owns the hardware.
Addresses are platform protocol addresses, not CPU MMIO.
Output is JSON register evidence, not optical/link readiness.";

#[derive(Debug)]
enum Command {
    Status,
    Init,
    CalibrateBosa(String, u64),
    Tx(bool),
    Fec(bool),
    Read(RegisterSpace, u32, u32, usize),
    Write(RegisterSpace, u32, u32, Vec<u32>),
    BosaRead(u32),
    BosaWrite(u32, u32),
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn number(value: &str) -> io::Result<u32> {
    if value.is_empty() || value.starts_with(['+', '-']) || value.trim() != value {
        return Err(invalid("invalid unsigned number"));
    }
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| invalid("invalid or overflowing number"))
}
fn parse(args: &[String]) -> io::Result<Command> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(invalid(USAGE));
    };
    match (command, args.len()) {
        ("status", 1) => Ok(Command::Status),
        ("init", 1) => Ok(Command::Init),
        ("calibrate-bosa", 1) => Ok(Command::CalibrateBosa(
            DEFAULT_BOSA_CAL_SOURCE.into(),
            DEFAULT_BOSA_CAL_OFFSET,
        )),
        ("calibrate-bosa", 2) => Ok(Command::CalibrateBosa(
            args[1].clone(),
            DEFAULT_BOSA_CAL_OFFSET,
        )),
        ("calibrate-bosa", 3) => Ok(Command::CalibrateBosa(
            args[1].clone(),
            u64::from(number(&args[2])?),
        )),
        ("bosa-read", 2) => Ok(Command::BosaRead(number(&args[1])?)),
        ("bosa-write", 3) => Ok(Command::BosaWrite(number(&args[1])?, number(&args[2])?)),
        ("tx" | "fec", 2) => {
            let enabled = match args[1].as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(invalid(USAGE)),
            };
            Ok(if command == "tx" {
                Command::Tx(enabled)
            } else {
                Command::Fec(enabled)
            })
        }
        ("reg-read" | "reg-write" | "mem-read" | "mem-write", _) => {
            let space = if command.starts_with("mem-") {
                RegisterSpace::Memory
            } else {
                RegisterSpace::Register
            };
            let offset = if space == RegisterSpace::Memory { 3 } else { 2 };
            if args.len() <= offset {
                return Err(invalid(USAGE));
            }
            let address = number(&args[1])?;
            let row = if space == RegisterSpace::Memory {
                number(&args[2])?
            } else {
                0
            };
            if command.ends_with("read") {
                if args.len() != offset + 1 {
                    return Err(invalid(USAGE));
                }
                let count = number(&args[offset])? as usize;
                if !(1..=16).contains(&count) {
                    return Err(invalid("word count must be 1..16"));
                }
                Ok(Command::Read(space, address, row, count))
            } else {
                if args.len() - offset > 16 {
                    return Err(invalid("word count must be 1..16"));
                }
                Ok(Command::Write(
                    space,
                    address,
                    row,
                    args[offset..]
                        .iter()
                        .map(|s| number(s))
                        .collect::<io::Result<_>>()?,
                ))
            }
        }
        _ => Err(invalid(USAGE)),
    }
}

pub(super) fn handle(args: &[String]) -> io::Result<()> {
    if args.len() == 1 && matches!(args[0].as_str(), "help" | "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() < 2 {
        return Err(invalid(USAGE));
    }
    let command = parse(&args[1..])?;
    let hardware = HardwareDiagnostics::detect().map_err(io::Error::other)?;
    match &command {
        Command::Read(space, address, row, count) => {
            hardware.validate_register_access(*space, *address, *row, *count)?;
        }
        Command::Write(space, address, row, words) => {
            hardware.validate_register_access(*space, *address, *row, words.len())?;
        }
        _ => {}
    }
    let _owner = openwrt_fttr::optical::claim_owner()?;
    let mut session = hardware.open_registers(Path::new(&args[0]))?;
    let evidence = match command {
        Command::Status => {
            let mac = session.pon_mac_readback()?;
            serde_json::json!({"tx_enabled": mac.tx_enabled, "downstream_fec_enabled": mac.fec_enabled,
                "raw_optical_register": mac.raw})
        }
        Command::Init => {
            let ev = session.initialize_olt()?;
            serde_json::json!({
                "onu_id_offset": ev.onu_id_offset,
                "serdes_status": ev.serdes_status,
                "ram_init_status": ev.ram_init_status,
                "nni_eth_tpid": ev.nni_eth_tpid,
                "nni_gem_port_offset": ev.nni_gem_port_offset,
                "vlan_type_cfg": ev.vlan_type_cfg,
            })
        }
        Command::CalibrateBosa(source, offset) => {
            // calibrate_bosa reopens the exclusive FSL device itself (keeping
            // TX off across every failure path); the register session opened
            // above would make that open fail with EBUSY.
            drop(session);
            let ev = hardware.calibrate_bosa(Path::new(&source), offset)?;
            serde_json::json!({
                "source": source,
                "offset": offset,
                "chip_id": ev.chip_id,
                "page": ev.page,
                "table_bytes": ev.table_bytes,
            })
        }
        Command::Read(space, address, row, count) => {
            serde_json::json!({"address": address, "row": row, "words": session.read(space, address, row, count)?})
        }
        Command::Write(space, address, row, words) => {
            session.write(space, address, row, &words)?;
            serde_json::json!({"address": address, "row": row, "written_words": words.len()})
        }
        Command::BosaRead(address) => {
            serde_json::json!({"address": address, "value": hardware.read_bosa_reg(address)?})
        }
        Command::BosaWrite(address, value) => {
            hardware.write_bosa_reg(address, value)?;
            serde_json::json!({"address": address, "value": value})
        }
        Command::Tx(enabled) => {
            session.set_optical_tx_enable(enabled)?;
            let mac = session.pon_mac_readback()?;
            serde_json::json!({"tx_enabled": enabled,
                "readback_tx_enabled": mac.tx_enabled,
                "raw_optical_register": mac.raw,
                "readback_verified": mac.tx_enabled == Some(enabled)})
        }
        Command::Fec(enabled) => {
            session.set_downstream_fec_enable(enabled)?;
            let mac = session.pon_mac_readback()?;
            serde_json::json!({"downstream_fec_enabled": enabled,
                "readback_fec_enabled": mac.fec_enabled,
                "readback_verified": mac.fec_enabled == Some(enabled)})
        }
    };
    println!(
        "{}",
        serde_json::json!({"status": "success", "scope": "register-evidence",
        "runtime_ready": false, "hardware_ready": null, "evidence": evidence})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn malformed_commands_fail_before_platform_selection_or_device_open() {
        for values in [
            vec!["reg-read", "0xxyz", "1"],
            vec!["reg-read", "0", "0"],
            vec!["reg-read", "0", "17"],
            vec!["status", "extra"],
            vec!["mem-read", "0", "1"],
            vec!["reg-write", "0"],
            vec!["tx", "yes"],
            vec!["reg-write", "0", "4294967296"],
            vec!["reg-write", "0", "-1"],
            vec!["reg-read", "+1", "1"],
        ] {
            assert!(parse(&args(&values)).is_err(), "{values:?}");
        }
        assert!(matches!(
            parse(&args(&["mem-read", "0x955", "255", "2"])).unwrap(),
            Command::Read(RegisterSpace::Memory, 0x955, 255, 2)
        ));
        assert!(matches!(
            parse(&args(&["reg-write", "0x102", "0X10"])).unwrap(),
            Command::Write(RegisterSpace::Register, 0x102, 0, _)
        ));
        assert!(matches!(
            parse(&args(&["calibrate-bosa"])).unwrap(),
            Command::CalibrateBosa(ref src, 0x1a_5000) if src == "/dev/ubi0_3"
        ));
        assert!(matches!(
            parse(&args(&["calibrate-bosa", "/tmp/cal.bin", "0x100"])).unwrap(),
            Command::CalibrateBosa(ref src, 0x100) if src == "/tmp/cal.bin"
        ));
    }
}
