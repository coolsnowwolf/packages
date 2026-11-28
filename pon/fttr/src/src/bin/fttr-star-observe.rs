use openwrt_fttr::platform::{context::PlatformContext, openwrt::packet::PacketSocket};
use std::{
    collections::BTreeMap,
    io,
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: fttr-star-observe SOURCE_MAC (90-second passive capture)",
        ));
    }
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected six colon-separated MAC octets",
        )
    };
    let octets = args[0]
        .split(':')
        .map(|octet| {
            if octet.len() != 2 || !octet.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            u8::from_str_radix(octet, 16).map_err(|_| invalid())
        })
        .collect::<io::Result<Vec<_>>>()?;
    let expected: [u8; 6] = octets.try_into().map_err(|_| invalid())?;
    let platform = PlatformContext::detect().map_err(io::Error::other)?;
    let interface = platform.resources().capture_interface.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "platform has no passive data capture interface",
        )
    })?;
    let socket = PacketSocket::open(interface)?;
    let start = Instant::now();
    let mut buffer = [0u8; 4096];
    let mut counts = BTreeMap::new();
    let mut star = 0u64;
    println!("PASSIVE duration=90s source_filter=configured no_transmit=true");
    while start.elapsed() < Duration::from_secs(90) {
        let Some((bytes, kind, aux)) = socket.receive_into(&mut buffer)? else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if kind == 4 || bytes.len() < 14 {
            continue;
        }
        let gem = aux.map(|tag| tag & 0xfff);
        *counts.entry(gem).or_insert(0u64) += 1;
        if bytes[6..12] != expected {
            continue;
        }
        star += 1;
        let mut offset = 12;
        let mut tags = Vec::new();
        while bytes.len() >= offset + 6
            && matches!(&bytes[offset..offset + 2], [0x81, 0] | [0x88, 0xa8])
        {
            tags.push(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
            offset += 4;
        }
        let ip = offset + 2;
        let dhcp = bytes.len() >= ip + 28
            && bytes[offset..offset + 2] == [8, 0]
            && bytes[ip + 9] == 17
            && {
                let udp = ip + usize::from(bytes[ip] & 15) * 4;
                bytes.len() >= udp + 4 && bytes[udp..udp + 4] == [0, 68, 0, 67]
            };
        if star <= 20 {
            let limit = if dhcp {
                bytes.len()
            } else {
                bytes.len().min(ip + 28)
            };
            let hex: String = bytes[..limit]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            println!("STAR elapsed_ms={} gem={gem:?} aux={aux:?} tags={tags:?} len={} dhcp={dhcp} hex={hex}", start.elapsed().as_millis(), bytes.len());
        }
    }
    println!("SUMMARY star_source_frames={star} received_envelopes={counts:?}");
    Ok(())
}
