//! Exact Netlink CTNETLINK conntrack cleanup for redirected TCP tuples.
use std::io;
use std::net::Ipv4Addr;

const NFNL_SUBSYS_CTNETLINK: u16 = 1;
const IPCTNL_MSG_CT_GET: u16 = 1;
const IPCTNL_MSG_CT_DELETE: u16 = 2;

const NLA_F_NESTED: u16 = 1 << 15;
const NLA_TYPE_MASK: u16 = !(1 << 15 | 1 << 14);

const CTA_TUPLE_ORIG: u16 = 1;
const CTA_TUPLE_REPLY: u16 = 2;
const CTA_ID: u16 = 12;

const CTA_TUPLE_IP: u16 = 1;
const CTA_TUPLE_PROTO: u16 = 2;

const CTA_IP_V4_SRC: u16 = 1;
const CTA_IP_V4_DST: u16 = 2;

const CTA_PROTO_NUM: u16 = 1;
const CTA_PROTO_SRC_PORT: u16 = 2;
const CTA_PROTO_DST_PORT: u16 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct NlMsgHdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NfGenMsg {
    nfgen_family: u8,
    version: u8,
    res_id: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Tuple4 {
    pub src_ip: Option<Ipv4Addr>,
    pub dst_ip: Option<Ipv4Addr>,
    pub proto: Option<u8>,
    pub src_port: Option<u16>,
    pub dst_port: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConntrackEntry {
    pub orig: Tuple4,
    pub reply: Tuple4,
    pub orig_nla_payload: Vec<u8>,
    pub id_nla_payload: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FlowMatcher {
    pub orig_src: Option<Ipv4Addr>,
    pub orig_dst: Ipv4Addr,
    pub orig_dst_prefix: u8,
    pub orig_port: u16,
    /// Required local DNAT reply endpoint `(ip, port)` so non-hijacked flows
    /// matching `orig_dst/orig_port` are never deleted.
    pub reply_target: (Ipv4Addr, u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FallbackSweep<'a> {
    pub reply_ip: Ipv4Addr,
    pub reply_port: u16,
    pub allowed_orig_ports: &'a [u16],
}

fn ip_in_cidr(ip: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    if prefix > 32 {
        return false;
    }
    let mask = if prefix == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

impl ConntrackEntry {
    pub(crate) fn matches_redirect(
        &self,
        matchers: &[FlowMatcher],
        fallback: &[FallbackSweep<'_>],
    ) -> bool {
        if self.orig.proto != Some(6) {
            return false;
        }
        let (Some(orig_src), Some(orig_dst), Some(orig_port)) =
            (self.orig.src_ip, self.orig.dst_ip, self.orig.dst_port)
        else {
            return false;
        };
        if matchers.iter().any(|m| {
            orig_port == m.orig_port
                && m.orig_src.is_none_or(|src| src == orig_src)
                && ip_in_cidr(orig_dst, m.orig_dst, m.orig_dst_prefix)
                && (orig_dst, orig_port) != m.reply_target
                && self.reply.src_ip == Some(m.reply_target.0)
                && self.reply.src_port == Some(m.reply_target.1)
        }) {
            return true;
        }
        // Crash/restart fallback: only delete flows that came from an allowed
        // original target port and were DNAT'd to our local endpoint.
        fallback.iter().any(|f| {
            f.allowed_orig_ports.contains(&orig_port)
                && (orig_dst, orig_port) != (f.reply_ip, f.reply_port)
                && self.reply.src_ip == Some(f.reply_ip)
                && self.reply.src_port == Some(f.reply_port)
        })
    }
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn push_nla(buf: &mut Vec<u8>, nla_type: u16, payload: &[u8]) {
    let total = 4 + payload.len();
    buf.extend_from_slice(&(total as u16).to_le_bytes());
    buf.extend_from_slice(&nla_type.to_le_bytes());
    buf.extend_from_slice(payload);
    let pad = align4(total) - total;
    buf.resize(buf.len() + pad, 0);
}

fn parse_nlas(mut bytes: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        let kind = u16::from_le_bytes([bytes[2], bytes[3]]) & NLA_TYPE_MASK;
        if len < 4 || len > bytes.len() {
            break;
        }
        out.push((kind, &bytes[4..len]));
        let step = align4(len);
        if step > bytes.len() {
            break;
        }
        bytes = &bytes[step..];
    }
    out
}

fn parse_tuple(bytes: &[u8]) -> Tuple4 {
    let mut tuple = Tuple4::default();
    for (kind, val) in parse_nlas(bytes) {
        match kind {
            CTA_TUPLE_IP => {
                for (ip_kind, ip_val) in parse_nlas(val) {
                    if ip_val.len() == 4 {
                        let ip = Ipv4Addr::new(ip_val[0], ip_val[1], ip_val[2], ip_val[3]);
                        match ip_kind {
                            CTA_IP_V4_SRC => tuple.src_ip = Some(ip),
                            CTA_IP_V4_DST => tuple.dst_ip = Some(ip),
                            _ => {}
                        }
                    }
                }
            }
            CTA_TUPLE_PROTO => {
                for (p_kind, p_val) in parse_nlas(val) {
                    match p_kind {
                        CTA_PROTO_NUM if p_val.len() == 1 => tuple.proto = Some(p_val[0]),
                        CTA_PROTO_SRC_PORT if p_val.len() == 2 => {
                            tuple.src_port = Some(u16::from_be_bytes([p_val[0], p_val[1]]));
                        }
                        CTA_PROTO_DST_PORT if p_val.len() == 2 => {
                            tuple.dst_port = Some(u16::from_be_bytes([p_val[0], p_val[1]]));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    tuple
}

pub(crate) fn parse_conntrack_payload(payload: &[u8]) -> Option<ConntrackEntry> {
    let hdr_size = std::mem::size_of::<NfGenMsg>();
    if payload.len() < hdr_size {
        return None;
    }
    let mut entry = ConntrackEntry {
        orig: Tuple4::default(),
        reply: Tuple4::default(),
        orig_nla_payload: Vec::new(),
        id_nla_payload: None,
    };
    for (kind, val) in parse_nlas(&payload[hdr_size..]) {
        match kind {
            CTA_TUPLE_ORIG => {
                entry.orig = parse_tuple(val);
                entry.orig_nla_payload = val.to_vec();
            }
            CTA_TUPLE_REPLY => entry.reply = parse_tuple(val),
            CTA_ID => entry.id_nla_payload = Some(val.to_vec()),
            _ => {}
        }
    }
    if entry.orig_nla_payload.is_empty() {
        None
    } else {
        Some(entry)
    }
}

fn build_request(msg: u16, flags: u16, seq: u32, attrs: &[u8]) -> Vec<u8> {
    let hdr_len = std::mem::size_of::<NlMsgHdr>();
    let gen_len = std::mem::size_of::<NfGenMsg>();
    let total = hdr_len + gen_len + attrs.len();
    let hdr = NlMsgHdr {
        nlmsg_len: total as u32,
        nlmsg_type: (NFNL_SUBSYS_CTNETLINK << 8) | msg,
        nlmsg_flags: flags,
        nlmsg_seq: seq,
        nlmsg_pid: 0,
    };
    let gen = NfGenMsg {
        nfgen_family: libc::AF_INET as u8,
        version: 0,
        res_id: 0,
    };
    let mut buf = Vec::with_capacity(total);
    unsafe {
        buf.extend_from_slice(std::slice::from_raw_parts(
            (&hdr as *const NlMsgHdr).cast::<u8>(),
            hdr_len,
        ));
        buf.extend_from_slice(std::slice::from_raw_parts(
            (&gen as *const NfGenMsg).cast::<u8>(),
            gen_len,
        ));
    }
    buf.extend_from_slice(attrs);
    buf
}

fn build_delete_attrs(entry: &ConntrackEntry) -> Vec<u8> {
    let mut attrs = Vec::new();
    push_nla(
        &mut attrs,
        CTA_TUPLE_ORIG | NLA_F_NESTED,
        &entry.orig_nla_payload,
    );
    if let Some(id) = &entry.id_nla_payload {
        push_nla(&mut attrs, CTA_ID, id);
    }
    attrs
}

struct NetlinkSocket {
    fd: i32,
}

impl Drop for NetlinkSocket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl NetlinkSocket {
    fn open() -> io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_NETFILTER,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sock = Self { fd };
        let tv = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&tv as *const libc::timeval).cast(),
                std::mem::size_of_val(&tv) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        if unsafe {
            libc::bind(
                fd,
                (&addr as *const libc::sockaddr_nl).cast(),
                std::mem::size_of_val(&addr) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(sock)
    }

    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        let sent = unsafe { libc::send(self.fd, bytes.as_ptr().cast(), bytes.len(), 0) };
        if sent < 0 {
            Err(io::Error::last_os_error())
        } else if sent as usize != bytes.len() {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short netlink send",
            ))
        } else {
            Ok(())
        }
    }

    fn recv_until_done(&self, expected_seq: u32) -> io::Result<Vec<ConntrackEntry>> {
        let mut entries = Vec::new();
        let mut buf = vec![0u8; 32768];
        let hdr_len = std::mem::size_of::<NlMsgHdr>();
        loop {
            let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut offset = 0usize;
            let limit = n as usize;
            while offset + hdr_len <= limit {
                let slice = &buf[offset..limit];
                let hdr = unsafe { std::ptr::read_unaligned(slice.as_ptr().cast::<NlMsgHdr>()) };
                let msg_len = hdr.nlmsg_len as usize;
                if msg_len < hdr_len || msg_len > slice.len() {
                    break;
                }
                if hdr.nlmsg_seq == expected_seq {
                    if hdr.nlmsg_type == libc::NLMSG_DONE as u16 {
                        return Ok(entries);
                    }
                    if hdr.nlmsg_type == libc::NLMSG_ERROR as u16 {
                        if msg_len >= hdr_len + 4 {
                            let err =
                                i32::from_ne_bytes(slice[hdr_len..hdr_len + 4].try_into().unwrap());
                            if err == 0 {
                                return Ok(entries);
                            }
                            return Err(io::Error::from_raw_os_error(-err));
                        }
                        return Err(io::Error::other("truncated netlink error"));
                    }
                    if let Some(entry) = parse_conntrack_payload(&slice[hdr_len..msg_len]) {
                        entries.push(entry);
                    }
                }
                let step = align4(msg_len);
                if step == 0 || offset + step > limit {
                    break;
                }
                offset += step;
            }
        }
    }
}

fn dump_matching(
    matchers: &[FlowMatcher],
    fallback: &[FallbackSweep<'_>],
) -> Result<Vec<ConntrackEntry>, String> {
    let sock = NetlinkSocket::open().map_err(|e| format!("open netfilter netlink: {e}"))?;
    let req = build_request(
        IPCTNL_MSG_CT_GET,
        (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16,
        1,
        &[],
    );
    sock.send(&req)
        .map_err(|e| format!("send conntrack dump: {e}"))?;
    let all = sock
        .recv_until_done(1)
        .map_err(|e| format!("read conntrack dump: {e}"))?;
    Ok(all
        .into_iter()
        .filter(|e| e.matches_redirect(matchers, fallback))
        .collect())
}

pub(crate) fn cleanup_redirected_flows(
    matchers: &[FlowMatcher],
    fallback: &[FallbackSweep<'_>],
) -> Result<usize, String> {
    let matching = dump_matching(matchers, fallback)?;
    if matching.is_empty() {
        return Ok(0);
    }
    let sock = NetlinkSocket::open().map_err(|e| format!("open netfilter netlink: {e}"))?;
    let mut deleted = 0usize;
    for (idx, entry) in matching.iter().enumerate() {
        let seq = (idx as u32) + 2;
        let attrs = build_delete_attrs(entry);
        let req = build_request(
            IPCTNL_MSG_CT_DELETE,
            (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16,
            seq,
            &attrs,
        );
        sock.send(&req)
            .map_err(|e| format!("send conntrack delete: {e}"))?;
        match sock.recv_until_done(seq) {
            Ok(_) => deleted += 1,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
            Err(e) => return Err(format!("conntrack delete failed: {e}")),
        }
    }
    let remaining = dump_matching(matchers, fallback)?;
    if !remaining.is_empty() {
        return Err(format!(
            "{} redirected conntrack flow(s) remained after cleanup",
            remaining.len()
        ));
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_tuple(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, sport: u16, dport: u16) -> Vec<u8> {
        let mut ip_nla = Vec::new();
        push_nla(&mut ip_nla, CTA_IP_V4_SRC, &src.octets());
        push_nla(&mut ip_nla, CTA_IP_V4_DST, &dst.octets());
        let mut proto_nla = Vec::new();
        push_nla(&mut proto_nla, CTA_PROTO_NUM, &[proto]);
        push_nla(&mut proto_nla, CTA_PROTO_SRC_PORT, &sport.to_be_bytes());
        push_nla(&mut proto_nla, CTA_PROTO_DST_PORT, &dport.to_be_bytes());
        let mut tuple = Vec::new();
        push_nla(&mut tuple, CTA_TUPLE_IP | NLA_F_NESTED, &ip_nla);
        push_nla(&mut tuple, CTA_TUPLE_PROTO | NLA_F_NESTED, &proto_nla);
        tuple
    }

    #[test]
    fn parses_conntrack_payload_and_matches_redirected_mqtt_and_acs_flows() {
        let client = Ipv4Addr::new(192, 0, 2, 244);
        let gw = Ipv4Addr::new(192, 0, 2, 1);
        let broker = Ipv4Addr::new(192, 0, 2, 44);
        let other = Ipv4Addr::new(192, 0, 2, 99);

        let orig = encode_tuple(client, gw, 6, 45000, 1883);
        let reply = encode_tuple(broker, client, 6, 1883, 45000);
        let mut payload = vec![libc::AF_INET as u8, 0, 0, 0];
        push_nla(&mut payload, CTA_TUPLE_ORIG | NLA_F_NESTED, &orig);
        push_nla(&mut payload, CTA_TUPLE_REPLY | NLA_F_NESTED, &reply);
        push_nla(&mut payload, CTA_ID, &[0x12, 0x34, 0x56, 0x78]);

        let parsed = parse_conntrack_payload(&payload).unwrap();
        assert_eq!(parsed.orig.src_ip, Some(client));
        assert_eq!(parsed.orig.dst_ip, Some(gw));
        assert_eq!(parsed.orig.dst_port, Some(1883));
        assert_eq!(parsed.reply.src_ip, Some(broker));
        let mqtt_matcher = FlowMatcher {
            orig_src: Some(client),
            orig_dst: gw,
            orig_dst_prefix: 32,
            orig_port: 1883,
            reply_target: (broker, 1883),
        };
        let mqtt_fallback = [FallbackSweep {
            reply_ip: broker,
            reply_port: 1883,
            allowed_orig_ports: &[1883],
        }];
        assert!(parsed.matches_redirect(&[mqtt_matcher], &mqtt_fallback));
        assert!(parsed.matches_redirect(&[], &mqtt_fallback));

        // Unrelated port DNAT targeting broker:1883 must NOT match MQTT fallback.
        let other_port_orig = encode_tuple(other, gw, 6, 45002, 8443);
        let other_port_reply = encode_tuple(broker, other, 6, 1883, 45002);
        let mut other_port_payload = vec![libc::AF_INET as u8, 0, 0, 0];
        push_nla(
            &mut other_port_payload,
            CTA_TUPLE_ORIG | NLA_F_NESTED,
            &other_port_orig,
        );
        push_nla(
            &mut other_port_payload,
            CTA_TUPLE_REPLY | NLA_F_NESTED,
            &other_port_reply,
        );
        let other_port = parse_conntrack_payload(&other_port_payload).unwrap();
        assert!(!other_port.matches_redirect(&[mqtt_matcher], &mqtt_fallback));

        // Direct connection to broker (non-redirected) must NOT match even with empty matchers.
        let direct_orig = encode_tuple(other, broker, 6, 45001, 1883);
        let direct_reply = encode_tuple(broker, other, 6, 1883, 45001);
        let mut direct_payload = vec![libc::AF_INET as u8, 0, 0, 0];
        push_nla(
            &mut direct_payload,
            CTA_TUPLE_ORIG | NLA_F_NESTED,
            &direct_orig,
        );
        push_nla(
            &mut direct_payload,
            CTA_TUPLE_REPLY | NLA_F_NESTED,
            &direct_reply,
        );
        let direct = parse_conntrack_payload(&direct_payload).unwrap();
        assert!(!direct.matches_redirect(&[mqtt_matcher], &mqtt_fallback));

        // Arbitrary CIDR + port ACS redirect match.
        let acs_target = Ipv4Addr::new(10, 20, 30, 40);
        let acs_orig = encode_tuple(client, acs_target, 6, 46000, 9090);
        let acs_reply = encode_tuple(broker, client, 6, 7547, 46000);
        let mut acs_payload = vec![libc::AF_INET as u8, 0, 0, 0];
        push_nla(&mut acs_payload, CTA_TUPLE_ORIG | NLA_F_NESTED, &acs_orig);
        push_nla(&mut acs_payload, CTA_TUPLE_REPLY | NLA_F_NESTED, &acs_reply);
        let acs_parsed = parse_conntrack_payload(&acs_payload).unwrap();
        let acs_matcher = FlowMatcher {
            orig_src: None,
            orig_dst: Ipv4Addr::new(10, 0, 0, 0),
            orig_dst_prefix: 8,
            orig_port: 9090,
            reply_target: (broker, 7547),
        };
        assert!(acs_parsed.matches_redirect(&[acs_matcher], &[]));
        assert!(!acs_parsed.matches_redirect(&[mqtt_matcher], &mqtt_fallback));

        // Unhijacked LAN/WAN flow to 10.20.30.40:9090 (reply.src == 10.20.30.40) must NEVER match.
        let direct_10_orig = encode_tuple(other, acs_target, 6, 46001, 9090);
        let direct_10_reply = encode_tuple(acs_target, other, 6, 9090, 46001);
        let mut direct_10_payload = vec![libc::AF_INET as u8, 0, 0, 0];
        push_nla(
            &mut direct_10_payload,
            CTA_TUPLE_ORIG | NLA_F_NESTED,
            &direct_10_orig,
        );
        push_nla(
            &mut direct_10_payload,
            CTA_TUPLE_REPLY | NLA_F_NESTED,
            &direct_10_reply,
        );
        let direct_10 = parse_conntrack_payload(&direct_10_payload).unwrap();
        assert!(!direct_10.matches_redirect(&[acs_matcher], &[]));
    }
}
