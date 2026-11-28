//! Explicit standard Linux i2c-dev endpoint; not the unreliable stock /dev/sif ABI.
use super::{Transfer, MAX_DATA_BYTES};
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::Path,
};

const I2C_FUNCS: libc::Ioctl = 0x0705;
const I2C_RDWR: libc::Ioctl = 0x0707;
const I2C_FUNC_I2C: libc::c_ulong = 1;
const I2C_M_RD: u16 = 1;

#[repr(C)]
struct Message {
    address: u16,
    flags: u16,
    len: u16,
    buffer: *mut u8,
}

#[repr(C)]
struct Transaction {
    messages: *mut Message,
    count: u32,
}

pub struct I2cDevice {
    file: File,
}

impl I2cDevice {
    /// Caller holds the common optical owner and has established adapter mapping,
    /// frequency and absence of another kernel/userspace consumer. No force mode.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut functions: libc::c_ulong = 0;
        // SAFETY: the writable native unsigned-long buffer outlives the ioctl.
        if unsafe { libc::ioctl(file.as_raw_fd(), I2C_FUNCS, &mut functions) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if functions & I2C_FUNC_I2C == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "adapter lacks combined I2C transfers",
            ));
        }
        Ok(Self { file })
    }
}

impl Transfer for I2cDevice {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> io::Result<usize> {
        if address > 0x7f
            || write.is_empty()
            || write.len() > MAX_DATA_BYTES + 1
            || read.len() > MAX_DATA_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid optical I2C message",
            ));
        }
        // i2c_msg has a mutable pointer even for writes. Keep a private writable
        // copy instead of casting away the Rust slice's shared-reference contract.
        let mut tx = [0; MAX_DATA_BYTES + 1];
        tx[..write.len()].copy_from_slice(write);
        let mut messages = [
            Message {
                address: address.into(),
                flags: 0,
                len: write.len() as u16,
                buffer: tx.as_mut_ptr(),
            },
            Message {
                address: address.into(),
                flags: I2C_M_RD,
                len: read.len() as u16,
                buffer: read.as_mut_ptr(),
            },
        ];
        let mut request = Transaction {
            messages: messages.as_mut_ptr(),
            count: if read.is_empty() { 1 } else { 2 },
        };
        // SAFETY: both native-layout structs and all pointed-to buffers remain
        // valid for the synchronous ioctl; counts/lengths are bounded above.
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_RDWR, &mut request) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn linux_uapi_layout_uses_native_pointers_not_fixed_compat_structs() {
        assert_eq!(offset_of!(Message, address), 0);
        assert_eq!(offset_of!(Message, flags), 2);
        assert_eq!(offset_of!(Message, len), 4);
        assert_eq!(offset_of!(Message, buffer), 8);
        assert_eq!(size_of::<Message>(), 8 + size_of::<*mut u8>());
        assert_eq!(align_of::<Message>(), align_of::<*mut u8>());
        assert_eq!(offset_of!(Transaction, messages), 0);
        assert_eq!(offset_of!(Transaction, count), size_of::<*mut Message>());
        assert_eq!(size_of::<Transaction>(), 2 * size_of::<*mut Message>());
    }

    #[test]
    fn rejects_non_i2c_endpoint_before_exposing_a_bus() {
        let error = I2cDevice::open(Path::new("/dev/null")).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
    }
}
