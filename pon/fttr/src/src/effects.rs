//! Common live-protocol admission, including standalone CLI processes.
//! Four fixed 8 MiB working-set reservations; the ACS response budget remains
//! separately charged to its consumer. These are reservations, not RSS limits.
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    sync::Arc,
};

pub(crate) const SLOTS: usize = 4;
pub(crate) const SLOT_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Command,
    Rpc,
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
        crate::provisioning::controller::wake();
    }
}
#[derive(Clone)]
pub(crate) struct Permit(Arc<Lease>);
impl Permit {
    pub(crate) fn acquire(kind: Kind) -> io::Result<Self> {
        if crate::runtime::is_stopping() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "effect admission stopped",
            ));
        }
        // Commands cannot consume the two response-first CWMP lanes. RPCs may
        // borrow idle command lanes; their FIFO/byte arbiter remains authoritative.
        let lanes: &[usize] = match kind {
            Kind::Command => &[0, 1],
            Kind::Rpc => &[2, 3, 0, 1],
        };
        for lane in lanes {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(format!("/tmp/fttr-effect-{lane}.lock"))?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(Arc::new(Lease(file))));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "protocol effect capacity exhausted",
        ))
    }
    pub(crate) fn bytes(&self) -> usize {
        let _ = &self.0;
        SLOT_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_capacity_preserves_rpc_lanes_and_releases_only_the_last_owner() {
        let a = Permit::acquire(Kind::Command).unwrap();
        let b = Permit::acquire(Kind::Command).unwrap();
        assert!(Permit::acquire(Kind::Command).is_err());
        let c = Permit::acquire(Kind::Rpc).unwrap();
        let d = Permit::acquire(Kind::Rpc).unwrap();
        assert_eq!(
            a.bytes() + b.bytes() + c.bytes() + d.bytes(),
            SLOTS * SLOT_BYTES
        );
        assert!(Permit::acquire(Kind::Rpc).is_err());
        let retained = a.clone();
        drop(a);
        assert!(Permit::acquire(Kind::Command).is_err());
        drop(retained);
        Permit::acquire(Kind::Command).unwrap();
        drop((b, c, d));
    }
}
