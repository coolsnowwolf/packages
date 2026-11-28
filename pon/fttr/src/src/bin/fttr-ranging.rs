//! Bounded hardware ranging diagnostic. Run with fttrd stopped (sole RX owner).
use openwrt_fttr::{platform::h3c_hm2004_du::fmcs as bosa, pon_engine::ploam::PloamFrame};
use std::{
    io, thread,
    time::{Duration, Instant},
};

fn send(frame: PloamFrame) -> io::Result<()> {
    let bytes = frame.to_bytes();
    if frame.message_id != 1 && frame.message_id != 0x14 {
        println!("TX {:02X?}", bytes);
    }
    bosa::send_ploam_msg(&bytes)
}

fn main() -> io::Result<()> {
    openwrt_fttr::platform::context::PlatformContext::detect().map_err(io::Error::other)?;
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: fttr-ranging SERIAL_HEX_16 ONU_ID_0_TO_15 SECONDS; stop fttrd first",
        ));
    }
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid argument");
    if args[1].len() != 16 || !args[1].is_ascii() {
        return Err(invalid());
    }
    let mut serial = [0u8; 8];
    for (i, b) in serial.iter_mut().enumerate() {
        *b = u8::from_str_radix(&args[1][i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
    }
    let id: u8 = args[2].parse().map_err(|_| invalid())?;
    let seconds: u64 = args[3].parse().map_err(|_| invalid())?;
    if id > 15 || seconds > 300 {
        return Err(invalid());
    }
    // Read event bits 16..31 from 0xb4 and map them to ONU-ID slots 0..15
    // at 0x01010020 + 4*(slot).
    let mask = 1u32 << (16 + id);
    let address = 0x01010020 + 4 * u32::from(id);
    bosa::configure_discovery_timing()?;
    // Protocol-level reactivation of this ID; no ONU power cycle required.
    for _ in 0..3 {
        send(PloamFrame::new(id, 5, [0; 10]))?;
    }
    thread::sleep(Duration::from_millis(100));
    // Zero seconds requests only protocol-level deactivation for a fresh trial.
    if seconds == 0 {
        return Ok(());
    }
    let baseline = bosa::read_fpga_reg(0xb4)?;
    println!(
        "BASE events=0x{baseline:08X} measurement=0x{:08X}",
        bosa::read_fpga_reg(address)?
    );
    for _ in 0..64 {
        if bosa::recv_ploam_msg()?.is_empty() {
            break;
        }
    }
    let start = Instant::now();
    let mut overhead = start - Duration::from_secs(1);
    let mut grant = overhead;
    let mut assigned: Option<Instant> = None;
    let mut cleared = baseline & mask == 0;
    let mut unicast = false;
    let mut measured = None;
    let mut ranged: Option<Instant> = None;
    // Send Assign_Alloc-ID with Alloc-ID=ONU-ID and type 1 (GPON).
    // Its exact acknowledgement is an independent O5 response.
    let mut alloc_payload = [0u8; 10];
    alloc_payload[0] = 1;
    alloc_payload[1] = id >> 4;
    alloc_payload[2] = id << 4;
    let alloc = PloamFrame::new(id, 0x0e, alloc_payload).to_bytes();
    let mut raw = 0;
    let mut valid = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        if assigned.is_none() && overhead.elapsed() >= Duration::from_millis(200) {
            for frame in openwrt_fttr::platform::h3c_hm2004_du::activation::discovery_frames() {
                send(frame)?;
            }
            overhead = Instant::now();
        }
        if grant.elapsed() >= Duration::from_millis(100) {
            let (target, length) = if assigned.is_some() {
                (id, (202 * 19440 + 124) / 125)
            } else {
                (0xfe, bosa::SERIAL_NUMBER_GRANT)
            };
            bosa::write_fpga_reg(
                bosa::FPGA_SERIAL_NUMBER_GRANT,
                (u32::from(target) << 18) | length,
            )?;
            grant = Instant::now();
        }
        let events = bosa::read_fpga_reg(0xb4)?;
        if events & mask == 0 {
            cleared = true;
        }
        if events != 0 {
            println!(
                "EVENT t={} bits=0x{events:08X}",
                start.elapsed().as_millis()
            );
        }
        if assigned.is_some() && ranged.is_none() && cleared && events & mask != 0 {
            let value = bosa::read_fpga_reg(address)?;
            println!("MEASURE id={id} address=0x{address:08X} bits={value}");
            if value != 0 {
                measured = Some(value);
            }
        }
        for _ in 0..16 {
            let bytes = bosa::recv_ploam_msg()?;
            if bytes.is_empty() {
                break;
            }
            raw += 1;
            let Some(frame) = PloamFrame::from_bytes(&bytes) else {
                println!("BAD_CRC {:02X?}", bytes);
                continue;
            };
            valid += 1;
            println!("RX t={} {:02X?}", start.elapsed().as_millis(), bytes);
            if frame.message_id == 1 && frame.payload[..8] == serial {
                if frame.onu_id == 0xff && assigned.is_none() {
                    let mut payload = [0u8; 10];
                    payload[0] = id;
                    payload[1..9].copy_from_slice(&serial);
                    for _ in 0..3 {
                        send(PloamFrame::new(0xff, 3, payload))?;
                    }
                    assigned = Some(Instant::now());
                    grant = start - Duration::from_secs(1);
                    println!("ASSIGNED id={id}");
                } else if frame.onu_id == id && assigned.is_some() {
                    unicast = true;
                    println!("MATCHED_UNICAST id={id}");
                }
            }
            if ranged.is_some() && frame.onu_id == id && frame.message_id == 2 {
                println!("POST_RANGING_PASSWORD id={id}: ONU has left O4");
                println!("RESULT ranging_complete=true raw={raw} valid={valid}");
                return Ok(());
            }
            if ranged.is_some()
                && frame.onu_id == id
                && frame.message_id == 9
                && frame.payload[0] == 0x0e
                && frame.payload[1..] == alloc[..9]
            {
                println!("POST_RANGING_ALLOC_ACK id={id}: exact Assign_Alloc-ID acknowledgement");
                println!("RESULT ranging_complete=true raw={raw} valid={valid}");
                return Ok(());
            }
        }
        if unicast && ranged.is_none() {
            if let Some(value) = measured {
                for _ in 0..3 {
                    send(PloamFrame::assign_delay(id, value))?;
                }
                ranged = Some(Instant::now());
                println!("RANGING_TIME_SENT id={id} eqd_bits={value} copies=3");
                thread::sleep(Duration::from_millis(40));
                for _ in 0..3 {
                    bosa::send_ploam_msg(&alloc)?;
                }
            }
        }
        if ranged.is_some() && grant.elapsed() < Duration::from_millis(10) {
            bosa::send_ploam_msg(&alloc)?;
        }
        if ranged.is_none() && assigned.map_or(false, |t| t.elapsed() > Duration::from_secs(10)) {
            println!("RETRY no valid ranging pair within 10s");
            assigned = None;
            unicast = false;
            measured = None;
        }
        thread::sleep(Duration::from_millis(10));
    }
    println!("RESULT ranging_complete=false raw={raw} valid={valid} unicast={unicast} measured={measured:?} ranging_time_sent={}", ranged.is_some());
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "ranging evidence incomplete",
    ))
}
