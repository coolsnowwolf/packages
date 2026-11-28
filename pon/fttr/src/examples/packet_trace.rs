//! Bounded, passive field diagnostic. Output can contain private packet data.
use openwrt_fttr::platform::openwrt::packet::PacketSocket;
use std::{
    io,
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "packet_trace INTERFACE SECONDS (1..300)",
        ));
    }
    let seconds: u64 = args[2].parse().map_err(io::Error::other)?;
    if !(1..=300).contains(&seconds) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seconds must be 1..300",
        ));
    }
    let socket = PacketSocket::open(&args[1])?;
    let start = Instant::now();
    let mut count = 0;
    while start.elapsed() < Duration::from_secs(seconds) && count < 20000 {
        if let Some((bytes, kind, vlan)) = socket.receive()? {
            count += 1;
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            println!(
                "{} kind={kind} vlan={vlan:?} {hex}",
                start.elapsed().as_millis()
            );
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}
