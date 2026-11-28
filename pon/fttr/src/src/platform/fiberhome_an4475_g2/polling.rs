//! Bounded register polling shared by FSL initialization and management TX.
use super::protocol::{Bus, Fsl};
use std::{io, num::NonZeroU16, time::Duration};

/// Software read budget, not a measured hardware timing guarantee.
#[derive(Clone, Copy, Debug)]
pub struct PollPolicy {
    pub max_reads: NonZeroU16,
    pub interval: Duration,
}

/// `checkpoint` runs before every access, with zero delay before the first
/// read and `interval` before subsequent reads. It must implement waiting,
/// cancellation and the caller's absolute deadline. No hardware operation is
/// retried on I/O error; the read budget also bounds a zero-interval policy.
pub(super) fn register<B: Bus>(
    fsl: &mut Fsl<B>,
    address: u32,
    mask: u32,
    expected: u32,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u32> {
    for attempt in 0..policy.max_reads.get() {
        checkpoint(if attempt == 0 {
            Duration::ZERO
        } else {
            policy.interval
        })?;
        let value = fsl.read_register(address)?;
        if value & mask == expected {
            return Ok(value);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "FSL register polling budget exhausted",
    ))
}
