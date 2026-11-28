//! Stock GPON management TX register protocol, independently testable without SPI.
//!
//! See fiberhome-fsl/docs/an4475-g2-management-protocol.md for evidence and limits.
//! The owner must initialize the device and serialize all management operations.
//! Hardware handshake completion does not prove delivery over the optical link.
use super::{
    management::Management,
    polling::{self, PollPolicy},
    protocol::Bus,
};
use crate::platform::diagnostics::RegisterSpace;
use std::{fmt, io, time::Duration};

pub const PLOAM_DATA: u32 = 0x403;
pub const PLOAM_INFO: u32 = 0x40f;
pub const OMCI_CONFIG: u32 = 0x913;
pub const OMCI_CONTROL: u32 = 0x916;
pub const OMCI_STATUS: u32 = 0x924;
pub const OMCI_MEMORY: u32 = 0x956;
/// Conservative stock SDK limit, not the 14-bit length field or device capacity.
pub const MAX_OMCI_BYTES: usize = 0x7bc;
const PLOAM_BUSY: u32 = 1 << 8;
const OMCI_BUSY: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    /// This call did not attempt a submit write. Preparation may have changed
    /// configuration or buffer contents; this does not mean "no side effects".
    NotSubmitted,
    /// A submit was attempted, but completion is unconfirmed. Do not replay.
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxPhase {
    Preparing,
    /// The frame has been submitted. Only observe its bounded completion;
    /// never submit it again when the preparation budget expires.
    Completing,
}

#[derive(Debug)]
pub struct SendError {
    pub submission: Submission,
    pub source: io::Error,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FSL management TX {:?}: {}",
            self.submission, self.source
        )
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Twelve software bytes, without the line-side CRC or the stock repeat wrapper.
/// Register words are low-to-high significance, unlike the software frame order.
pub fn pack_ploam(frame: &[u8; 12]) -> [u32; 12] {
    let mut words = [0; 12];
    words[0] = u32::from_be_bytes([frame[9], frame[10], frame[11], 0]);
    words[1] = u32::from_be_bytes(frame[5..9].try_into().unwrap());
    words[2] = u32::from_be_bytes(frame[1..5].try_into().unwrap());
    words[3] = u32::from(frame[0]);
    words
}

/// Memory rows are logical low/high words; Fsl::write performs memory word
/// reversal. Partial final rows are padded with zero, never previous frame data.
fn omci_row(bytes: &[u8]) -> [u32; 2] {
    let mut row = [0; 8];
    row[..bytes.len()].copy_from_slice(bytes);
    [
        u32::from_be_bytes(row[4..8].try_into().unwrap()),
        u32::from_be_bytes(row[..4].try_into().unwrap()),
    ]
}

impl<B: Bus> Management<B> {
    /// Enqueue once. Required protocol repetition belongs to the caller and
    /// must not be confused with replay after an ambiguous I/O failure.
    pub fn send_ploam(
        &mut self,
        frame: &[u8; 12],
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> Result<(), SendError> {
        let result = (|| {
            self.ensure_ready()?;
            if self.read(PLOAM_INFO, checkpoint)? & PLOAM_BUSY != 0 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "PLOAM TX busy"));
            }
            checkpoint(Duration::ZERO)?;
            self.fsl
                .write(RegisterSpace::Register, PLOAM_DATA, 0, &pack_ploam(frame))?;
            self.submit(PLOAM_INFO, 0x101, checkpoint)?;
            polling::register(&mut self.fsl, PLOAM_INFO, PLOAM_BUSY, 0, policy, checkpoint)?;
            self.indeterminate = false;
            Ok(())
        })();
        self.finish(result)
    }

    /// Raw OMCI/WMCI selector is the descriptor's bit27, not an ONU-ID. The
    /// caller supplies its verified selection, GEM Port-ID and opaque bytes;
    /// OMCI framing/CRC policy and ONU-to-GEM mapping belong to the upper layer.
    pub fn send_omci_frame(
        &mut self,
        port_id: u16,
        selection: bool,
        frame: &[u8],
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> Result<(), SendError> {
        self.send_omci_frame_with_phases(port_id, selection, frame, policy, &mut |_, delay| {
            checkpoint(delay)
        })
    }

    pub fn send_omci_frame_with_phases(
        &mut self,
        port_id: u16,
        selection: bool,
        frame: &[u8],
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(TxPhase, Duration) -> io::Result<()>,
    ) -> Result<(), SendError> {
        let result = (|| {
            let mut prepare = |delay| checkpoint(TxPhase::Preparing, delay);
            self.ensure_ready()?;
            if port_id > 0xfff || !(1..=MAX_OMCI_BYTES).contains(&frame.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid FSL management port or frame length",
                ));
            }
            // Added idle preflight: never overwrite a known busy buffer.
            if self.read(OMCI_STATUS, &mut prepare)? & OMCI_BUSY != 0 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "OMCI TX busy"));
            }
            // Stock GPON path clears CRC mode bit1, then enables SPI bits7/0.
            // Combine independent fields into one RMW, preserving all others.
            let old = self.read(OMCI_CONFIG, &mut prepare)?;
            let config = (old & !2) | 0x81;
            if old != config {
                self.write(OMCI_CONFIG, config, &mut prepare)?;
            }
            if self.read(OMCI_CONFIG, &mut prepare)? & 0x83 != 0x81 {
                return Err(io::Error::other("FSL OMCI configuration readback mismatch"));
            }
            for (row, bytes) in frame.chunks(8).enumerate() {
                prepare(Duration::ZERO)?;
                self.fsl.write(
                    RegisterSpace::Memory,
                    OMCI_MEMORY,
                    row as u32,
                    &omci_row(bytes),
                )?;
            }
            let control = ((selection as u32) << 27)
                | ((frame.len() as u32) << 13)
                | (u32::from(port_id) << 1);
            self.write(OMCI_CONTROL, control, &mut prepare)?;
            self.submit(OMCI_CONTROL, control | 1, &mut prepare)?;
            polling::register(
                &mut self.fsl,
                OMCI_STATUS,
                OMCI_BUSY,
                0,
                policy,
                &mut |delay| checkpoint(TxPhase::Completing, delay),
            )?;
            self.indeterminate = false;
            Ok(())
        })();
        self.finish(result)
    }

    fn finish(&self, result: io::Result<()>) -> Result<(), SendError> {
        result.map_err(|source| SendError {
            submission: if self.indeterminate {
                Submission::Indeterminate
            } else {
                Submission::NotSubmitted
            },
            source,
        })
    }
}

#[cfg(test)]
mod tests;
