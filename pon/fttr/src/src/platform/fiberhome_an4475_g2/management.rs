//! One owner for management TX, RX, cursor state and ambiguous side effects.
use super::protocol::{Bus, Fsl};
use crate::platform::diagnostics::RegisterSpace;
use std::{io, time::Duration};

/// Keep this session for the entire initialized device generation. Recreating
/// it does not reconcile an ambiguous submit/consume or synchronize hardware.
pub struct Management<B> {
    pub(super) fsl: Fsl<B>,
    pub(super) indeterminate: bool,
    pub(super) omci_cursor: Option<u8>,
}

impl<B: Bus> Management<B> {
    /// OMCI reception remains unavailable until the initial ring cursor is known.
    pub fn new(fsl: Fsl<B>) -> Self {
        Self {
            fsl,
            indeterminate: false,
            omci_cursor: None,
        }
    }

    /// The owner must establish this cursor through verified initialization or
    /// handover. Zero is the stock software-reset value, not a live-state probe.
    pub fn with_omci_cursor(fsl: Fsl<B>, cursor: u8) -> Self {
        Self {
            omci_cursor: Some(cursor),
            ..Self::new(fsl)
        }
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.indeterminate
    }

    pub fn omci_cursor(&self) -> Option<u8> {
        self.omci_cursor
    }

    pub(super) fn ensure_ready(&self) -> io::Result<()> {
        if self.indeterminate {
            Err(io::Error::other(
                "previous FSL submit/consume requires reconciliation",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn read(
        &mut self,
        address: u32,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<u32> {
        checkpoint(Duration::ZERO)?;
        self.fsl.read_register(address)
    }

    pub(super) fn write(
        &mut self,
        address: u32,
        value: u32,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<()> {
        checkpoint(Duration::ZERO)?;
        self.fsl
            .write(RegisterSpace::Register, address, 0, &[value])
    }

    pub(super) fn submit(
        &mut self,
        address: u32,
        value: u32,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<()> {
        checkpoint(Duration::ZERO)?;
        // Even a failing ioctl may already have transmitted the write.
        self.indeterminate = true;
        self.fsl
            .write(RegisterSpace::Register, address, 0, &[value])
    }
}
