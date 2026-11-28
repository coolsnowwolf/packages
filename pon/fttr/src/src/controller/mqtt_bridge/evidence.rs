//! Bounded DHCP transaction evidence for bridge-mode MQTT discovery.
//!
//! A lease is not an ONU identity and is not, by itself, authorization to
//! redirect traffic. Callers must independently establish the client's identity,
//! current ONU port and the origin of the DHCP server.
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const MAX_PENDING: usize = 256;
const REQUEST_WINDOW: Duration = Duration::from_secs(30);
const MAX_PACKET: usize = 4096;
const COOKIE: [u8; 4] = [99, 130, 83, 99];

pub(crate) use super::super::redirect_net::lan::LanOrigin as Origin;

/// Independently established DHCP server origins. Missing addresses are unknown;
/// absence from a local-server list is not evidence of an upstream server.
pub type ServerOrigins = BTreeMap<Ipv4Addr, Origin>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lease {
    pub client: [u8; 6],
    pub address: Ipv4Addr,
    pub server: Ipv4Addr,
    pub gateway: Option<Ipv4Addr>,
    pub origin: Origin,
    pub expires: Instant,
}

/// A minimally validated, unfragmented Ethernet/IPv4/UDP DHCP datagram.
/// The capture owner must additionally validate interface, VLAN and packet
/// direction against the current ONU registration and trusted LAN path.
pub struct DhcpFrame<'a> {
    pub source: Ipv4Addr,
    pub ethernet_source: [u8; 6],
    pub client: [u8; 6],
    pub request: bool,
    pub payload: &'a [u8],
}

/// A minimally validated, unfragmented Ethernet/IPv4/TCP pure SYN targeting MQTT (1883).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MqttSyn {
    pub ethernet_source: [u8; 6],
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
}

pub fn decode_mqtt_syn(frame: &[u8]) -> Option<MqttSyn> {
    if frame.len() < 14 + 20 + 20 || frame[12..14] != [0x08, 0x00] {
        return None;
    }
    let ethernet_source: [u8; 6] = frame[6..12].try_into().ok()?;
    if ethernet_source[0] & 1 != 0 || ethernet_source == [0; 6] {
        return None;
    }
    let header = &frame[14..];
    if header[0] >> 4 != 4 || header[9] != 6 {
        return None;
    }
    let header_len = usize::from(header[0] & 15) * 4;
    let total = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let fragment = u16::from_be_bytes([header[6], header[7]]);
    if header_len < 20 || total < header_len + 20 || total > header.len() || fragment & 0x3fff != 0
    {
        return None;
    }
    let source = ip(&header[12..16])?;
    let destination = ip(&header[16..20])?;
    if !usable(source) || !usable(destination) || source == destination {
        return None;
    }
    let tcp = &header[header_len..total];
    let source_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    let dest_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    let data_offset = usize::from(tcp[12] >> 4) * 4;
    let flags = tcp[13];
    if source_port == 0
        || dest_port != 1883
        || data_offset < 20
        || data_offset > tcp.len()
        || (flags & 0x17) != 0x02
    {
        return None;
    }
    Some(MqttSyn {
        ethernet_source,
        source,
        destination,
    })
}

/// Kernel cBPF filter for per-ONU `fttrN` bridge-port sockets:
/// - incoming (`kind != PACKET_OUTGOING`): IPv4 UDP dst 67 (DHCP Request) or TCP dst 1883 pure SYN
/// - outgoing (`kind == PACKET_OUTGOING`): IPv4 UDP dst 68 (DHCP ACK)
/// Every other packet (including established MQTT traffic and bulk data) is dropped in-kernel.
pub(crate) fn onu_capture_program() -> Vec<libc::sock_filter> {
    let ins = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    const PACKET_OUTGOING: u32 = 4;
    const SKF_AD_PKTTYPE: u32 = (-4096i32 + 4) as u32;
    vec![
        ins(0x28, 0, 0, 12),               //  0: ldh [12] (EtherType)
        ins(0x15, 0, 22, 0x0800),          //  1: jeq #0x0800, +0, drop(24)
        ins(0x30, 0, 0, 14),               //  2: ldb [14] (IPv4 ver+ihl)
        ins(0x15, 0, 20, 0x45),            //  3: jeq #0x45, +0, drop(24)
        ins(0x28, 0, 0, 20),               //  4: ldh [20] (IPv4 frag)
        ins(0x45, 18, 0, 0x3fff),          //  5: jset #0x3fff, drop(24), +0
        ins(0x30, 0, 0, 23),               //  6: ldb [23] (IPv4 proto)
        ins(0x15, 8, 0, 17),               //  7: jeq #17 (UDP), udp(16), +0
        ins(0x15, 0, 15, 6),               //  8: jeq #6 (TCP), +0, drop(24)
        ins(0x20, 0, 0, SKF_AD_PKTTYPE),   //  9: ld [SKF_AD_PKTTYPE]
        ins(0x15, 13, 0, PACKET_OUTGOING), // 10: jeq #PACKET_OUTGOING, drop(24), +0
        ins(0x28, 0, 0, 36),               // 11: ldh [36] (TCP dst port)
        ins(0x15, 0, 11, 1883),            // 12: jeq #1883, +0, drop(24)
        ins(0x30, 0, 0, 47),               // 13: ldb [47] (TCP flags)
        ins(0x54, 0, 0, 0x17),             // 14: and #0x17 (FIN|SYN|RST|ACK)
        ins(0x15, 7, 8, 0x02),             // 15: jeq #0x02, accept(23), drop(24)
        ins(0x28, 0, 0, 36),               // 16: ldh [36] (UDP dst port)
        ins(0x15, 1, 0, 67),               // 17: jeq #67, check_in(19), +0
        ins(0x15, 2, 5, 68),               // 18: jeq #68, check_out(21), drop(24)
        ins(0x20, 0, 0, SKF_AD_PKTTYPE),   // 19: ld [SKF_AD_PKTTYPE]
        ins(0x15, 3, 2, PACKET_OUTGOING),  // 20: jeq #PACKET_OUTGOING, drop(24), accept(23)
        ins(0x20, 0, 0, SKF_AD_PKTTYPE),   // 21: ld [SKF_AD_PKTTYPE]
        ins(0x15, 0, 1, PACKET_OUTGOING),  // 22: jeq #PACKET_OUTGOING, accept(23), drop(24)
        ins(0x06, 0, 0, u32::MAX),         // 23: ret #u32::MAX
        ins(0x06, 0, 0, 0),                // 24: ret #0
    ]
}

pub(crate) fn attach_onu_capture_filter(fd: i32) -> std::io::Result<()> {
    let mut code = onu_capture_program();
    let program = libc::sock_fprog {
        len: code.len() as u16,
        filter: code.as_mut_ptr(),
    };
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ATTACH_FILTER,
            &program as *const _ as *const libc::c_void,
            std::mem::size_of_val(&program) as libc::socklen_t,
        )
    };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn decode_frame(frame: &[u8]) -> Option<DhcpFrame<'_>> {
    if frame.len() < 14 + 20 + 8 || frame[12..14] != [0x08, 0x00] {
        return None;
    }
    let ethernet_source: [u8; 6] = frame[6..12].try_into().ok()?;
    let header = &frame[14..];
    if header[0] >> 4 != 4 || header[9] != 17 {
        return None;
    }
    let header_len = usize::from(header[0] & 15) * 4;
    let total = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let fragment = u16::from_be_bytes([header[6], header[7]]);
    if header_len < 20 || total < header_len + 8 || total > header.len() || fragment & 0x3fff != 0 {
        return None;
    }
    let udp = &header[header_len..total];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let dest_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    if udp_len != udp.len() || udp_len < 8 {
        return None;
    }
    let payload = &udp[8..];
    let packet = parse(payload)?;
    let request = match (source_port, dest_port, packet.op, packet.message) {
        (68, 67, 1, 3) if ethernet_source == packet.client => true,
        (67, 68, 2, 5) => false,
        _ => return None,
    };
    Some(DhcpFrame {
        source: ip(&header[12..16])?,
        ethernet_source,
        client: packet.client,
        request,
        payload,
    })
}

#[derive(Clone, Copy)]
struct Request {
    address: Ipv4Addr,
    server: Option<Ipv4Addr>,
    seen: Instant,
}

#[derive(Default)]
pub struct Transactions {
    pending: BTreeMap<([u8; 6], u32), Request>,
}

struct Packet {
    op: u8,
    xid: u32,
    client: [u8; 6],
    ciaddr: Ipv4Addr,
    yiaddr: Ipv4Addr,
    message: u8,
    requested: Option<Ipv4Addr>,
    server: Option<Ipv4Addr>,
    gateway: Option<Ipv4Addr>,
    lease: Option<u32>,
}

fn ip(bytes: &[u8]) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::new(
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ))
}

fn parse(data: &[u8]) -> Option<Packet> {
    if !(240..=MAX_PACKET).contains(&data.len())
        || data[1] != 1
        || data[2] != 6
        || data[3] != 0
        || data[24..28] != [0; 4]
        || data[236..240] != COOKIE
    {
        return None;
    }
    let client: [u8; 6] = data[28..34].try_into().ok()?;
    if client[0] & 1 != 0 || client == [0; 6] {
        return None;
    }
    let mut message = None;
    let mut requested = None;
    let mut server = None;
    let mut gateway = None;
    let mut lease = None;
    let mut end = false;
    let mut pos = 240;
    while pos < data.len() {
        let code = data[pos];
        pos += 1;
        if code == 0 {
            continue;
        }
        if code == 255 {
            end = true;
            break;
        }
        let len = usize::from(*data.get(pos)?);
        pos += 1;
        let value = data.get(pos..pos.checked_add(len)?)?;
        pos += len;
        match code {
            53 if len == 1 && message.is_none() => message = Some(value[0]),
            50 if len == 4 && requested.is_none() => requested = ip(value),
            54 if len == 4 && server.is_none() => server = ip(value),
            3 if len == 4 && gateway.is_none() => gateway = ip(value),
            51 if len == 4 && lease.is_none() => {
                lease = Some(u32::from_be_bytes(value.try_into().ok()?))
            }
            // Option overload and ambiguous identity/routing options are not evidence.
            3 | 50 | 51 | 52 | 53 | 54 => return None,
            _ => {}
        }
    }
    if !end {
        return None;
    }
    Some(Packet {
        op: data[0],
        xid: u32::from_be_bytes(data[4..8].try_into().ok()?),
        client,
        ciaddr: ip(&data[12..16])?,
        yiaddr: ip(&data[16..20])?,
        message: message?,
        requested,
        server,
        gateway,
        lease,
    })
}

fn usable(ip: Ipv4Addr) -> bool {
    !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast()
}

impl Transactions {
    /// Observe a DHCP payload. `source` is the IP source of the received packet.
    /// Requests and ACKs must be observed on the same trusted capture path.
    pub fn observe(
        &mut self,
        data: &[u8],
        source: Ipv4Addr,
        now: Instant,
        origins: &ServerOrigins,
    ) -> Option<Lease> {
        self.pending.retain(|_, request| {
            now.checked_duration_since(request.seen)
                .is_some_and(|age| age <= REQUEST_WINDOW)
        });
        let p = parse(data)?;
        let key = (p.client, p.xid);
        if p.op == 1 && p.message == 3 {
            // A selecting or INIT-REBOOT request uses 0.0.0.0; a renewal
            // uses its existing address. An unrelated IP source must not
            // create a transaction that a later ACK can authorize.
            if (p.ciaddr.is_unspecified() && !source.is_unspecified())
                || (!p.ciaddr.is_unspecified() && (!usable(p.ciaddr) || source != p.ciaddr))
            {
                return None;
            }
            if p.requested
                .is_some_and(|requested| usable(p.ciaddr) && requested != p.ciaddr)
            {
                return None;
            }
            let address = p
                .requested
                .or_else(|| usable(p.ciaddr).then_some(p.ciaddr))?;
            if !usable(address) || p.server.is_some_and(|s| !usable(s)) {
                return None;
            }
            if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING {
                return None;
            }
            self.pending.insert(
                key,
                Request {
                    address,
                    server: p.server,
                    seen: now,
                },
            );
            return None;
        }
        if p.op != 2 || p.message != 5 {
            return None;
        }
        let request = self.pending.remove(&key)?;
        let server = p.server.filter(|s| usable(*s))?;
        // An infinite lease has no observable expiry and cannot authorize a
        // long-lived redirect under the lease-expiry policy.
        let seconds = p.lease.filter(|s| *s > 0 && *s != u32::MAX)?;
        // Relayed ACKs need a separate verified server mapping; fail closed here.
        if source != server
            || p.yiaddr != request.address
            || request.server.is_some_and(|s| s != server)
            || p.gateway.is_some_and(|g| !usable(g))
        {
            return None;
        }
        let expires = now.checked_add(Duration::from_secs(u64::from(seconds)))?;
        let origin = origins.get(&server).copied().unwrap_or(Origin::Unknown);
        Some(Lease {
            client: p.client,
            address: p.yiaddr,
            server,
            gateway: p.gateway,
            origin,
            expires,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(op: u8, kind: u8, xid: u32, address: Ipv4Addr, server: Ipv4Addr) -> Vec<u8> {
        let mut p = vec![0; 240];
        p[0] = op;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&xid.to_be_bytes());
        p[28..34].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        p[236..240].copy_from_slice(&COOKIE);
        if op == 2 {
            p[16..20].copy_from_slice(&address.octets());
        }
        p.extend_from_slice(&[53, 1, kind]);
        if op == 1 {
            p.extend_from_slice(&[50, 4]);
            p.extend_from_slice(&address.octets());
        }
        p.extend_from_slice(&[54, 4]);
        p.extend_from_slice(&server.octets());
        if op == 2 {
            p.extend_from_slice(&[51, 4, 0, 0, 0, 60, 3, 4]);
            p.extend_from_slice(&server.octets());
        }
        p.push(255);
        p
    }

    fn frame(payload: &[u8], request: bool) -> Vec<u8> {
        let mut frame = vec![0; 14 + 20 + 8];
        frame[6..12].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        frame[12..14].copy_from_slice(&[8, 0]);
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
        frame[23] = 17;
        let (src, dst): (u16, u16) = if request { (68, 67) } else { (67, 68) };
        frame[34..36].copy_from_slice(&src.to_be_bytes());
        frame[36..38].copy_from_slice(&dst.to_be_bytes());
        frame[38..40].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn frame_decoder_rejects_unsafe_or_unrelated_packets() {
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let req = packet(1, 3, 7, address, server);
        let good = frame(&req, true);
        assert!(decode_frame(&good).unwrap().request);
        let ack = packet(2, 5, 7, address, server);
        assert!(!decode_frame(&frame(&ack, false)).unwrap().request);
        for offset in [6, 12, 20, 23, 34, 38] {
            let mut bad = good.clone();
            bad[offset] ^= 1;
            assert!(decode_frame(&bad).is_none(), "bad byte at {offset}");
        }
        assert!(decode_frame(&good[..good.len() - 1]).is_none());
    }

    #[test]
    fn correlated_lease_requires_explicit_server_origin() {
        let now = Instant::now();
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let request = packet(1, 3, 7, address, server);
        let ack = packet(2, 5, 7, address, server);
        let mut t = Transactions::default();
        assert!(t
            .observe(&request, Ipv4Addr::UNSPECIFIED, now, &ServerOrigins::new())
            .is_none());
        let lease = t.observe(&ack, server, now, &ServerOrigins::new()).unwrap();
        assert_eq!(lease.origin, Origin::Unknown);
        assert_eq!(lease.expires.duration_since(now), Duration::from_secs(60));
        assert!(t
            .observe(&ack, server, now, &ServerOrigins::new())
            .is_none());
        t.observe(&request, Ipv4Addr::UNSPECIFIED, now, &ServerOrigins::new());
        let unrelated = ServerOrigins::from([(Ipv4Addr::new(192, 0, 2, 2), Origin::Local)]);
        assert_eq!(
            t.observe(&ack, server, now, &unrelated).unwrap().origin,
            Origin::Unknown
        );
        t.observe(&request, Ipv4Addr::UNSPECIFIED, now, &ServerOrigins::new());
        let local = ServerOrigins::from([(server, Origin::Local)]);
        assert_eq!(
            t.observe(&ack, server, now, &local).unwrap().origin,
            Origin::Local
        );
        t.observe(&request, Ipv4Addr::UNSPECIFIED, now, &local);
        let upstream = ServerOrigins::from([(server, Origin::Upstream)]);
        assert_eq!(
            t.observe(&ack, server, now, &upstream).unwrap().origin,
            Origin::Upstream
        );
    }

    #[test]
    fn rejects_unmatched_stale_relayed_and_ambiguous_packets() {
        let now = Instant::now();
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let req = packet(1, 3, 7, address, server);
        let ack = packet(2, 5, 7, address, server);
        let mut t = Transactions::default();
        assert!(t
            .observe(&ack, server, now, &ServerOrigins::new())
            .is_none());
        t.observe(&req, Ipv4Addr::UNSPECIFIED, now, &ServerOrigins::new());
        assert!(t
            .observe(
                &ack,
                server,
                now + REQUEST_WINDOW + Duration::from_secs(1),
                &ServerOrigins::new()
            )
            .is_none());
        t.observe(&req, Ipv4Addr::UNSPECIFIED, now, &ServerOrigins::new());
        assert!(t
            .observe(&ack, address, now, &ServerOrigins::new())
            .is_none());
        let mut duplicate = req.clone();
        duplicate.pop();
        duplicate.extend_from_slice(&[53, 1, 3, 255]);
        assert!(parse(&duplicate).is_none());
        let mut truncated = ack.clone();
        truncated.truncate(truncated.len() - 2);
        assert!(parse(&truncated).is_none());
        let mut relayed = ack.clone();
        relayed[24..28].copy_from_slice(&server.octets());
        assert!(parse(&relayed).is_none());
    }

    #[test]
    fn renewal_and_server_or_address_mismatch() {
        let now = Instant::now();
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let other = Ipv4Addr::new(192, 0, 2, 2);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let mut renewal = packet(1, 3, 8, address, server);
        renewal[12..16].copy_from_slice(&address.octets());
        // Renewal uses ciaddr rather than the requested-address option.
        renewal.drain(243..249);
        let ack = packet(2, 5, 8, address, server);
        let mut t = Transactions::default();
        t.observe(&renewal, address, now, &ServerOrigins::new());
        assert!(t
            .observe(&ack, server, now, &ServerOrigins::new())
            .is_some());

        t.observe(&renewal, address, now, &ServerOrigins::new());
        let wrong_address = packet(2, 5, 8, other, server);
        assert!(t
            .observe(&wrong_address, server, now, &ServerOrigins::new())
            .is_none());
        t.observe(&renewal, address, now, &ServerOrigins::new());
        let wrong_server = packet(2, 5, 8, address, other);
        assert!(t
            .observe(&wrong_server, other, now, &ServerOrigins::new())
            .is_none());
    }

    #[test]
    fn rejects_requests_with_inconsistent_ip_source() {
        let now = Instant::now();
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let request = packet(1, 3, 7, address, server);
        let ack = packet(2, 5, 7, address, server);
        let mut t = Transactions::default();
        t.observe(&request, address, now, &ServerOrigins::new());
        assert!(t
            .observe(&ack, server, now, &ServerOrigins::new())
            .is_none());

        let mut renewal = request;
        renewal[12..16].copy_from_slice(&address.octets());
        renewal.drain(243..249);
        t.observe(&renewal, server, now, &ServerOrigins::new());
        assert!(t
            .observe(&ack, server, now, &ServerOrigins::new())
            .is_none());
    }

    #[test]
    fn pending_transactions_are_bounded_and_expire() {
        let now = Instant::now();
        let server = Ipv4Addr::new(192, 0, 2, 1);
        let address = Ipv4Addr::new(192, 0, 2, 20);
        let mut t = Transactions::default();
        for xid in 0..(MAX_PENDING as u32 + 10) {
            t.observe(
                &packet(1, 3, xid, address, server),
                Ipv4Addr::UNSPECIFIED,
                now,
                &ServerOrigins::new(),
            );
        }
        assert_eq!(t.pending.len(), MAX_PENDING);
        let later = now + REQUEST_WINDOW + Duration::from_secs(1);
        t.observe(
            &packet(1, 3, 999, address, server),
            Ipv4Addr::UNSPECIFIED,
            later,
            &ServerOrigins::new(),
        );
        assert_eq!(t.pending.len(), 1);
    }

    fn tcp_syn_frame(
        src_mac: [u8; 6],
        src: Ipv4Addr,
        dst: Ipv4Addr,
        dst_port: u16,
        flags: u8,
    ) -> Vec<u8> {
        let mut f = vec![0; 14 + 20 + 20];
        f[6..12].copy_from_slice(&src_mac);
        f[12..14].copy_from_slice(&[8, 0]);
        f[14] = 0x45;
        f[16..18].copy_from_slice(&40u16.to_be_bytes());
        f[23] = 6;
        f[26..30].copy_from_slice(&src.octets());
        f[30..34].copy_from_slice(&dst.octets());
        f[34..36].copy_from_slice(&45000u16.to_be_bytes());
        f[36..38].copy_from_slice(&dst_port.to_be_bytes());
        f[46] = 0x50;
        f[47] = flags;
        f
    }

    #[test]
    fn mqtt_syn_decoder_and_cbpf_accept_only_expected_control_frames() {
        let mac = [2, 0, 0, 0, 0, 1];
        let src = Ipv4Addr::new(192, 0, 2, 20);
        let dst = Ipv4Addr::new(192, 0, 2, 1);
        let syn = tcp_syn_frame(mac, src, dst, 1883, 0x02);
        assert_eq!(
            decode_mqtt_syn(&syn),
            Some(MqttSyn {
                ethernet_source: mac,
                source: src,
                destination: dst,
            })
        );
        assert!(decode_mqtt_syn(&tcp_syn_frame(mac, src, dst, 1883, 0x12)).is_none());
        assert!(decode_mqtt_syn(&tcp_syn_frame(mac, src, dst, 443, 0x02)).is_none());
        assert!(
            decode_mqtt_syn(&tcp_syn_frame([1, 0, 0, 0, 0, 1], src, dst, 1883, 0x02)).is_none()
        );

        let accepts = |bytes: &[u8], kind: u32| -> bool {
            let code = onu_capture_program();
            let (mut pc, mut a) = (0usize, 0u32);
            loop {
                let i = &code[pc];
                pc += 1;
                match i.code {
                    0x20 if i.k == (-4096i32 + 4) as u32 => a = kind,
                    0x28 | 0x30 => {
                        let w = if i.code == 0x28 { 2 } else { 1 };
                        let Some(slice) = bytes.get(i.k as usize..i.k as usize + w) else {
                            return false;
                        };
                        a = slice.iter().fold(0, |acc, b| (acc << 8) | u32::from(*b));
                    }
                    0x54 => a &= i.k,
                    0x15 => pc += if a == i.k { i.jt } else { i.jf } as usize,
                    0x45 => pc += if a & i.k != 0 { i.jt } else { i.jf } as usize,
                    0x06 => return i.k != 0,
                    _ => panic!("unexpected opcode {:#x}", i.code),
                }
            }
        };

        assert!(accepts(&syn, 0));
        assert!(!accepts(&syn, 4));
        assert!(!accepts(&tcp_syn_frame(mac, src, dst, 1883, 0x10), 0));
        assert!(!accepts(&tcp_syn_frame(mac, src, dst, 80, 0x02), 0));
        let dhcp_req = frame(&packet(1, 3, 1, src, dst), true);
        assert!(accepts(&dhcp_req, 0));
        assert!(!accepts(&dhcp_req, 4));
        let dhcp_ack = frame(&packet(2, 5, 1, src, dst), false);
        assert!(!accepts(&dhcp_ack, 0));
        assert!(accepts(&dhcp_ack, 4));
    }

    #[test]
    fn linux_kernel_accepts_onu_capture_filter() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;
        let (_tx, rx) = UnixDatagram::pair().unwrap();
        attach_onu_capture_filter(rx.as_raw_fd()).unwrap();
    }
}
