//! Coalescing Linux worker notifications. State/queues remain authoritative.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    time::Instant,
};

#[derive(Debug)]
pub(crate) struct Wake(OwnedFd);

impl Wake {
    pub(crate) fn new() -> io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
        }
    }

    pub(crate) fn notify(&self) {
        let value = 1u64;
        loop {
            let result =
                unsafe { libc::write(self.0.as_raw_fd(), &value as *const _ as *const _, 8) };
            if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                // EAGAIN means a notification is already pending.
                break;
            }
        }
    }

    // Drain BEFORE checking state, never between checking state and waiting.
    pub(crate) fn drain(&self) {
        let mut value = 0u64;
        loop {
            let result =
                unsafe { libc::read(self.0.as_raw_fd(), &mut value as *mut _ as *mut _, 8) };
            if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }

    pub(crate) fn wait(&self, sockets: &[RawFd], deadline: Instant) -> io::Result<()> {
        let mut fds = [libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        }; 4];
        assert!(sockets.len() < fds.len());
        for (slot, fd) in fds
            .iter_mut()
            .zip(std::iter::once(self.0.as_raw_fd()).chain(sockets.iter().copied()))
        {
            slot.fd = fd;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let millis = remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
        let result = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                millis.min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if fds
            .iter()
            .any(|fd| fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "health socket or notification closed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn notification_before_wait_is_retained_and_coalesced() {
        let wake = Wake::new().unwrap();
        wake.notify();
        wake.notify();
        wake.wait(&[], Instant::now() + Duration::from_secs(30))
            .unwrap();
        wake.drain();
        let start = Instant::now();
        wake.wait(&[], start + Duration::from_millis(20)).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn blocked_wait_wakes_for_notifications_and_socket_input() {
        let wake = Arc::new(Wake::new().unwrap());
        let sender = wake.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            sender.notify();
        });
        wake.wait(&[], Instant::now() + Duration::from_secs(30))
            .unwrap();
        thread.join().unwrap();
        wake.drain();
        let (tx, rx) = std::os::unix::net::UnixDatagram::pair().unwrap();
        tx.send(b"packet").unwrap();
        wake.wait(&[rx.as_raw_fd()], Instant::now() + Duration::from_secs(30))
            .unwrap();
        let mut buf = [0; 6];
        rx.recv(&mut buf).unwrap();
        assert_eq!(&buf, b"packet");
    }
}
