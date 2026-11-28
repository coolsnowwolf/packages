//! Dedicated thin-driver UAPI; each request is one continuous-CS SPI message.
use super::protocol::Bus;
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::Path,
    time::Duration,
};

const MAX_TRANSFER: usize = 128;
const fn ioc(direction: u32, kind: u8, number: u8, size: usize) -> libc::Ioctl {
    ((direction << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | number as u32)
        as libc::Ioctl
}

// Inline arrays, no userspace pointers: identical native/compat layout. Keep in
// sync with fiberhome-fsl/src/fsl-uapi.h (fixed-size version 1 command).
#[repr(C)]
struct Transfer {
    tx_len: u32,
    rx_len: u32,
    reserved: [u32; 2],
    tx: [u8; MAX_TRANSFER],
    rx: [u8; MAX_TRANSFER],
}
const FSL_TRANSFER: libc::Ioctl = ioc(3, b'F', 1, std::mem::size_of::<Transfer>());

pub(super) trait Ioctl {
    fn call<T>(
        &mut self,
        fd: libc::c_int,
        request: libc::Ioctl,
        value: &mut T,
    ) -> io::Result<libc::c_int>;
}
pub(super) struct LinuxIoctl;
impl Ioctl for LinuxIoctl {
    fn call<T>(
        &mut self,
        fd: libc::c_int,
        request: libc::Ioctl,
        value: &mut T,
    ) -> io::Result<libc::c_int> {
        let result = unsafe { libc::ioctl(fd, request, value as *mut T) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }
}

pub(super) struct Device<I = LinuxIoctl> {
    file: File,
    ioctl: I,
}
impl Device {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        // The dedicated driver enforces exclusive open and configures SPI.
        Ok(Self {
            file,
            ioctl: LinuxIoctl,
        })
    }
}
impl<I: Ioctl> Device<I> {
    fn message(&mut self, tx: &[u8], rx: &mut [u8]) -> io::Result<()> {
        if tx.is_empty() || tx.len() + rx.len() > MAX_TRANSFER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FSL SPI message exceeds UAPI bounds",
            ));
        }
        let mut transfer = Transfer {
            tx_len: tx.len() as u32,
            rx_len: rx.len() as u32,
            reserved: [0; 2],
            tx: [0; MAX_TRANSFER],
            rx: [0; MAX_TRANSFER],
        };
        transfer.tx[..tx.len()].copy_from_slice(tx);
        if self
            .ioctl
            .call(self.file.as_raw_fd(), FSL_TRANSFER, &mut transfer)?
            != 0
        {
            return Err(io::Error::other("unexpected FSL UAPI result"));
        }
        rx.copy_from_slice(&transfer.rx[..rx.len()]);
        Ok(())
    }
}
impl<I: Ioctl> Bus for Device<I> {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> io::Result<()> {
        let result = self.message(tx, rx);
        // Historical inter-message pacing, also on failure. Never replay a
        // transaction with an unknown outcome.
        std::thread::sleep(Duration::from_micros(100));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct MockIoctl {
        calls: usize,
        fail: bool,
        short: bool,
    }
    impl Ioctl for MockIoctl {
        fn call<T>(
            &mut self,
            _: libc::c_int,
            request: libc::Ioctl,
            arg: &mut T,
        ) -> io::Result<libc::c_int> {
            self.calls += 1;
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "mock ioctl"));
            }
            assert_eq!(request, 0xc1104601_u32 as libc::Ioctl);
            let transfer = unsafe { &mut *(arg as *mut T as *mut Transfer) };
            assert_eq!(transfer.reserved, [0, 0]);
            assert_eq!(transfer.tx_len, 6);
            assert_eq!(transfer.rx_len, 4);
            transfer.rx[..4].copy_from_slice(&[1, 2, 3, 4]);
            Ok(if self.short { 1 } else { 0 })
        }
    }
    fn device() -> Device<MockIoctl> {
        Device {
            file: File::open("/dev/null").unwrap(),
            ioctl: MockIoctl {
                calls: 0,
                fail: false,
                short: false,
            },
        }
    }
    #[test]
    fn uapi_layout_is_fixed_and_read_uses_one_message() {
        assert_eq!(std::mem::size_of::<Transfer>(), 272);
        assert_eq!(FSL_TRANSFER, 0xc1104601_u32 as libc::Ioctl);
        let mut device = device();
        let mut rx = [0; 4];
        device.transfer(&[0; 6], &mut rx).unwrap();
        assert_eq!(rx, [1, 2, 3, 4]);
        assert_eq!(device.ioctl.calls, 1);
    }
    #[test]
    fn short_io_timeouts_and_oversize_are_rejected_without_replay() {
        let mut device = device();
        device.ioctl.short = true;
        assert!(device.transfer(&[0; 6], &mut [0; 4]).is_err());
        assert_eq!(device.ioctl.calls, 1);
        device.ioctl.fail = true;
        assert_eq!(
            device.transfer(&[0; 6], &mut [0; 4]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(device.ioctl.calls, 2);
        assert_eq!(
            device.transfer(&[0; 129], &mut []).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(device.ioctl.calls, 2);
    }
}
