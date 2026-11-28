//! H3C kernel admission filters for production health sockets.
use std::{io, mem};

#[derive(Clone, Copy)]
pub(super) enum Capture {
    Closed,
    Omci(u8),
    Gem(u8),
}

fn ins(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

impl Capture {
    fn program(self) -> Vec<libc::sock_filter> {
        if matches!(self, Self::Closed) {
            return vec![ins(0x06, 0, 0, 0)];
        }
        // Ancillary loads use Linux SKF_AD_OFF offsets; VLAN metadata is
        // essential when the NIC/kernel has already removed the outer tag.
        let mut code = vec![
            ins(0x20, 0, 0, (-4096i32 + 4) as u32), // packet type
            ins(0x15, 0, 1, 4),                     // PACKET_OUTGOING
            ins(0x06, 0, 0, 0),                     // reject
        ];
        code.extend(match self {
            Self::Closed => unreachable!(),
            Self::Omci(id) => vec![
                ins(0x80, 0, 0, 0), // packet length
                ins(0x15, 0, 5, 56),
                ins(0x20, 0, 0, 0), // envelope type
                ins(0x15, 0, 3, 3),
                ins(0x20, 0, 0, 4), // ONU ID
                ins(0x15, 0, 1, id as u32),
                ins(0x06, 0, 0, u32::MAX), // retain full packet
                ins(0x06, 0, 0, 0),
            ],
            Self::Gem(id) => vec![
                ins(0x20, 0, 0, (-4096i32 + 48) as u32), // VLAN present
                ins(0x15, 2, 0, 0),
                ins(0x20, 0, 0, (-4096i32 + 44) as u32), // VLAN TCI
                ins(0x05, 0, 0, 3),
                ins(0x28, 0, 0, 12), // inline outer VLAN
                ins(0x15, 0, 3, 0x8100),
                ins(0x28, 0, 0, 14),
                ins(0x15, 0, 1, 0x100 + id as u32),
                ins(0x06, 0, 0, u32::MAX),
                ins(0x06, 0, 0, 0),
            ],
        });
        code
    }

    pub(super) fn attach(self, fd: i32) -> io::Result<()> {
        let mut code = self.program();
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
                mem::size_of_val(&program) as libc::socklen_t,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise ancillary metadata paths as well as packet bytes. Ordinary
    // datagram injection cannot synthesize hardware-stripped VLAN metadata.
    fn accepts(filter: Capture, bytes: &[u8], kind: u32, vlan: Option<u16>) -> bool {
        let code = filter.program();
        let (mut pc, mut a) = (0, 0);
        loop {
            let i = &code[pc];
            pc += 1;
            match i.code {
                0x20 if i.k == (-4096i32 + 4) as u32 => a = kind,
                0x20 if i.k == (-4096i32 + 48) as u32 => a = vlan.is_some() as u32,
                0x20 if i.k == (-4096i32 + 44) as u32 => a = vlan.unwrap_or(0) as u32,
                0x20 | 0x28 => {
                    let width = if i.code == 0x20 { 4 } else { 2 };
                    let Some(data) = bytes.get(i.k as usize..i.k as usize + width) else {
                        return false;
                    };
                    a = data.iter().fold(0, |n, b| (n << 8) | u32::from(*b));
                }
                0x80 => a = bytes.len() as u32,
                0x15 => pc += if a == i.k { i.jt } else { i.jf } as usize,
                0x05 => pc += i.k as usize,
                0x06 => return i.k != 0,
                _ => panic!("unsupported filter instruction"),
            }
        }
    }

    #[test]
    fn health_filters_isolate_onus_directions_and_vlan_representations() {
        for id in [0, 1, 15] {
            let frame = super::super::fmcs::envelope(
                id,
                &crate::pon_engine::omci::wire::BaselineFrame::new(1, 0x29, 256, 0, [0; 32]),
            );
            assert!(accepts(Capture::Omci(id), &frame, 0, None));
            assert!(!accepts(Capture::Omci(id), &frame, 4, None));
            assert!(!accepts(Capture::Omci(id ^ 1), &frame, 0, None));
            assert!(!accepts(Capture::Omci(id), &frame[..55], 0, None));
            let mut invalid = frame;
            invalid[3] = 2;
            assert!(!accepts(Capture::Omci(id), &invalid, 0, None));

            let mut ethernet = [0; 68];
            ethernet[12..16].copy_from_slice(&[0x81, 0, 1, id]);
            assert!(accepts(Capture::Gem(id), &ethernet, 0, None));
            ethernet[12..14].copy_from_slice(&[8, 0]);
            assert!(accepts(
                Capture::Gem(id),
                &ethernet,
                0,
                Some(0x100 + id as u16)
            ));
            assert!(!accepts(
                Capture::Gem(id),
                &ethernet,
                4,
                Some(0x100 + id as u16)
            ));
            assert!(!accepts(
                Capture::Gem(id ^ 1),
                &ethernet,
                0,
                Some(0x100 + id as u16)
            ));
            assert!(!accepts(
                Capture::Gem(id),
                &ethernet,
                0,
                Some(0x2100 + id as u16)
            ));
            assert!(!accepts(Capture::Gem(id), &ethernet, 0, None));
            assert!(!accepts(Capture::Gem(id), &ethernet[..13], 0, None));
        }
    }

    #[test]
    fn linux_verifies_and_executes_capture_programs() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;
        for filter in [Capture::Omci(15), Capture::Gem(15)] {
            let (tx, rx) = UnixDatagram::pair().unwrap();
            rx.set_nonblocking(true).unwrap();
            filter.attach(rx.as_raw_fd()).unwrap();
            let mut valid = vec![0; 68];
            match filter {
                Capture::Closed => unreachable!(),
                Capture::Omci(_) => {
                    valid.resize(56, 0);
                    valid[3] = 3;
                    valid[7] = 15;
                }
                Capture::Gem(_) => valid[12..16].copy_from_slice(&[0x81, 0, 1, 15]),
            }
            let mut buf = [0; 128];
            tx.send(&valid).unwrap();
            assert_eq!(rx.recv(&mut buf).unwrap(), valid.len());
            assert_eq!(&buf[..valid.len()], valid);
            valid[if matches!(filter, Capture::Omci(_)) {
                7
            } else {
                15
            }] = 14;
            tx.send(&valid).unwrap();
            assert_eq!(
                rx.recv(&mut buf).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            // Once current-session GEM is proved, even valid traffic stops waking health.
            valid[if matches!(filter, Capture::Omci(_)) {
                7
            } else {
                15
            }] = 15;
            Capture::Closed.attach(rx.as_raw_fd()).unwrap();
            tx.send(&valid).unwrap();
            assert_eq!(
                rx.recv(&mut buf).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }
}
