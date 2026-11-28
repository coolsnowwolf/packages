//! Linux AF_PACKET transport. No PON envelope or hardware interface policy.
use std::{ffi::CString, io};

#[repr(C)]
struct Address {
    family: u16,
    protocol: u16,
    index: i32,
    hatype: u16,
    kind: u8,
    length: u8,
    bytes: [u8; 8],
}
#[repr(C)]
struct IoVec {
    base: *mut u8,
    length: usize,
}
#[repr(C)]
struct MsgHdr {
    name: *mut Address,
    name_length: u32,
    iov: *mut IoVec,
    iov_length: usize,
    control: *mut usize,
    control_length: usize,
    flags: i32,
}
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, address: *const Address, length: u32) -> i32;
    fn send(fd: i32, buffer: *const u8, length: usize, flags: i32) -> isize;
    fn sendto(
        fd: i32,
        buffer: *const u8,
        length: usize,
        flags: i32,
        address: *const Address,
        address_length: u32,
    ) -> isize;
    fn recvmsg(fd: i32, message: *mut MsgHdr, flags: i32) -> isize;
    fn setsockopt(fd: i32, level: i32, option: i32, value: *const i32, length: u32) -> i32;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
}
/// One RAII packet socket for both sending and capturing, including GEM VLAN evidence.
pub struct PacketSocket {
    fd: i32,
    index: i32,
}
impl Drop for PacketSocket {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
    }
}
impl PacketSocket {
    pub(crate) fn raw_fd(&self) -> i32 {
        self.fd
    }
    pub fn open(name: &str) -> io::Result<Self> {
        Self::open_filtered(name, false, |_| Ok(()))
    }
    pub(crate) fn open_filtered(
        name: &str,
        ignore_outgoing: bool,
        attach: impl FnOnce(i32) -> io::Result<()>,
    ) -> io::Result<Self> {
        let name = CString::new(name).map_err(io::Error::other)?;
        let index = unsafe { if_nametoindex(name.as_ptr()) } as i32;
        if index == 0 {
            return Err(io::Error::last_os_error());
        }
        // Protocol zero prevents reception before binding and installing the filter.
        let fd = unsafe { socket(17, 3 | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = Self { fd, index };
        let enabled = 1;
        if unsafe { setsockopt(fd, 263, 8, &enabled, 4) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if ignore_outgoing {
            // PACKET_IGNORE_OUTGOING avoids the tap work; the filter also rejects
            // echoes, independently of this packet-socket optimization.
            if unsafe { setsockopt(fd, 263, 23, &enabled, 4) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        attach(fd)?;
        let address = Address {
            family: 17,
            protocol: 3u16.to_be(),
            index,
            hatype: 0,
            kind: 0,
            length: 0,
            bytes: [0; 8],
        };
        if unsafe { bind(fd, &address, std::mem::size_of::<Address>() as u32) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }
    pub fn transmit(&self, bytes: &[u8]) -> io::Result<()> {
        check_send(
            unsafe { send(self.fd, bytes.as_ptr(), bytes.len(), 0) },
            bytes.len(),
        )
    }
    pub(crate) fn send_broadcast(&self, bytes: &[u8]) -> io::Result<()> {
        let address = Address {
            family: 17,
            protocol: 0,
            index: self.index,
            hatype: 0,
            kind: 0,
            length: 6,
            bytes: [0xff; 8],
        };
        check_send(
            unsafe {
                sendto(
                    self.fd,
                    bytes.as_ptr(),
                    bytes.len(),
                    0,
                    &address,
                    std::mem::size_of::<Address>() as u32,
                )
            },
            bytes.len(),
        )
    }
    pub fn receive_into<'a>(
        &self,
        buffer: &'a mut [u8],
    ) -> io::Result<Option<(&'a [u8], u8, Option<u16>)>> {
        let mut address: Address = unsafe { std::mem::zeroed() };
        let mut control = [0usize; 16];
        let mut iov = IoVec {
            base: buffer.as_mut_ptr(),
            length: buffer.len(),
        };
        let mut message = MsgHdr {
            name: &mut address,
            name_length: std::mem::size_of::<Address>() as u32,
            iov: &mut iov,
            iov_length: 1,
            control: control.as_mut_ptr(),
            control_length: std::mem::size_of_val(&control),
            flags: 0,
        };
        let count = unsafe { recvmsg(self.fd, &mut message, 0x40) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error);
        }
        let valid_len = (count as usize).min(buffer.len());
        let raw = unsafe {
            std::slice::from_raw_parts(
                control.as_ptr() as *const u8,
                message.control_length.min(std::mem::size_of_val(&control)),
            )
        };
        let header = std::mem::size_of::<usize>() + 8;
        let vlan = if raw.len() >= header + 20
            && i32::from_ne_bytes(raw[8..12].try_into().unwrap()) == 263
            && i32::from_ne_bytes(raw[12..16].try_into().unwrap()) == 8
        {
            let status = u32::from_ne_bytes(raw[header..header + 4].try_into().unwrap());
            if status & (1 << 4) != 0 {
                Some(u16::from_ne_bytes(
                    raw[header + 16..header + 18].try_into().unwrap(),
                ))
            } else {
                None
            }
        } else {
            None
        };
        let bytes = &buffer[..valid_len];
        let vlan = vlan.or_else(|| {
            (bytes.len() >= 16 && bytes[12..14] == [0x81, 0])
                .then(|| u16::from_be_bytes([bytes[14], bytes[15]]))
        });
        Ok(Some((bytes, address.kind, vlan)))
    }

    pub fn receive(&self) -> io::Result<Option<(Vec<u8>, u8, Option<u16>)>> {
        let mut buffer = [0; 2048];
        match self.receive_into(&mut buffer)? {
            Some((bytes, kind, vlan)) => Ok(Some((bytes.to_vec(), kind, vlan))),
            None => Ok(None),
        }
    }
}
fn check_send(count: isize, expected: usize) -> io::Result<()> {
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count as usize != expected {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short packet send",
        ));
    }
    Ok(())
}
