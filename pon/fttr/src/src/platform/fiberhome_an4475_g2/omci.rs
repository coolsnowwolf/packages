//! `OmciTransport` adapter over shared FiberHome FSL `Management` ownership.
//!
//! NOT used by the AN4475-G2 backend: `platform::backend` wires
//! `runtime::FslOmciTransport` (session-scoped queues, `selection = true`).
//! This module stays for two reasons: (1) it is the only integration test
//! driving `Management` OMCI send/receive end to end through `OmciTransport`,
//! and (2) it documents the raw (`selection = false`) path as a reference
//! implementation — whether the OMCI downstream selector bit is 0 or 1 is an
//! unresolved hardware question (see downstream.rs on descriptor bit 27).
//!
//! Sends 48-byte ITU-T G.984.4 baseline frames (`BaselineFrame::to_bytes`) on
//! the raw OMCI (`selection = false`) path and decodes matching `OmciKind::Omci`
//! upstream frames. Ambiguous (`Indeterminate`) SPI/handshake errors propagate
//! without retry so callers never replay uncertain submissions.

use super::{
    downstream::SendError,
    management::Management,
    polling::PollPolicy,
    protocol::Bus,
    upstream::{OmciKind, ReceiveError},
};
use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
use std::{
    io,
    num::NonZeroU16,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const MAX_PORT_ID: u16 = 0xfff;

/// Adapts [`Management<B>`] to the PON engine [`OmciTransport`] trait.
pub struct FslOmciTransport<B> {
    management: Arc<Mutex<Management<B>>>,
    omcc_port_id: u16,
    source_filter: Option<u8>,
    tx_policy: PollPolicy,
    rx_policy: PollPolicy,
    cancel: Option<Arc<AtomicBool>>,
}

impl<B: Bus> FslOmciTransport<B> {
    /// Creates an OMCI transport bound to `omcc_port_id` (12-bit GEM Port-ID).
    pub fn new(
        management: Arc<Mutex<Management<B>>>,
        omcc_port_id: u16,
        tx_policy: PollPolicy,
    ) -> io::Result<Self> {
        if omcc_port_id > MAX_PORT_ID {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FSL OMCC GEM Port-ID exceeds 12-bit hardware range",
            ));
        }
        Ok(Self {
            management,
            omcc_port_id,
            source_filter: None,
            tx_policy,
            rx_policy: PollPolicy {
                max_reads: NonZeroU16::MIN,
                interval: Duration::ZERO,
            },
            cancel: None,
        })
    }

    /// Filters upstream frames by the raw 8-bit header `source` field (`bits 21:14`).
    pub fn with_source_filter(mut self, source: Option<u8>) -> Self {
        self.source_filter = source;
        self
    }

    /// Overrides the single-probe default receive policy when polling hardware status.
    pub fn with_rx_policy(mut self, rx_policy: PollPolicy) -> Self {
        self.rx_policy = rx_policy;
        self
    }

    /// Attaches a cancellation token checked before and between polling reads.
    pub fn with_cancellation(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    fn checkpoint(&self, delay: Duration) -> io::Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "FSL OMCI transport operation cancelled",
            ));
        }
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        Ok(())
    }
}

fn lock_error() -> io::Error {
    io::Error::other("FSL management mutex poisoned")
}

fn send_io_error(err: SendError) -> io::Error {
    io::Error::new(err.source.kind(), err.to_string())
}

fn receive_io_error(err: ReceiveError) -> io::Error {
    io::Error::new(err.source.kind(), err.to_string())
}

impl<B: Bus + Send> OmciTransport for FslOmciTransport<B> {
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        self.checkpoint(Duration::ZERO)?;
        let bytes = frame.to_bytes();
        let mut mgmt = self.management.lock().map_err(|_| lock_error())?;
        mgmt.send_omci_frame(self.omcc_port_id, false, &bytes, self.tx_policy, &mut |d| {
            self.checkpoint(d)
        })
        .map_err(send_io_error)
    }

    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        self.checkpoint(Duration::ZERO)?;
        let frame = {
            let mut mgmt = self.management.lock().map_err(|_| lock_error())?;
            mgmt.receive_omci_frame(self.rx_policy, &mut |d| self.checkpoint(d))
                .map_err(receive_io_error)?
        };
        let Some(frame) = frame else {
            return Ok(None);
        };
        if frame.kind != OmciKind::Omci {
            return Ok(None);
        }
        if self
            .source_filter
            .is_some_and(|expected| frame.source != expected)
        {
            return Ok(None);
        }
        BaselineFrame::from_bytes(&frame.payload).map(Some)
    }
}

#[cfg(test)]
mod tests;
