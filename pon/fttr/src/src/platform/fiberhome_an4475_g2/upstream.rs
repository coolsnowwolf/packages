//! Bounded stock GPON management RX and explicit queue consumption.
//! See fiberhome-fsl/docs/an4475-g2-management-protocol.md for static evidence.
use super::{downstream::OMCI_STATUS, management::Management, polling::PollPolicy, protocol::Bus};
use crate::platform::diagnostics::RegisterSpace;
use std::{fmt, io, time::Duration};

pub const PLOAM_VALID: u32 = 0x41b;
pub const PLOAM_LAST: u32 = 0x41c;
pub const PLOAM_DATA: u32 = 0x490;
pub const OMCI_MARKER: u32 = 0x917;
pub const OMCI_CONSUME: u32 = 0x921;
pub const OMCI_MEMORY: u32 = 0x955;
pub const OMCI_RING_ROWS: usize = 256;
/// Stock raw receiver limit; upper-layer event delivery has a smaller SDK limit.
pub const MAX_OMCI_BYTES: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PloamRecord {
    /// All 416 bits are retained; metadata and unused tail are not discarded.
    pub words: [u32; 13],
    /// Stock GPON byte layout, including its thirteenth (line CRC) byte.
    /// This is not a claim that the CRC or any message semantics are valid.
    pub frame: [u8; 13],
    pub last: bool,
}

impl PloamRecord {
    pub fn decode(words: [u32; 13], last: bool) -> Self {
        let mut frame = [0; 13];
        frame[0] = words[4] as u8;
        frame[1..5].copy_from_slice(&words[3].to_be_bytes());
        frame[5..9].copy_from_slice(&words[2].to_be_bytes());
        frame[9..13].copy_from_slice(&words[1].to_be_bytes());
        Self { words, frame, last }
    }

    /// The stock dispatcher skips records with metadata bit4 set. Its precise
    /// physical cause is unproven, so do not name it a CRC/MIC error.
    pub fn stock_error_flag(&self) -> bool {
        self.words[0] & (1 << 4) != 0
    }

    /// Upstream rate metadata extracted by `fsldrv_gpon_ploamu_receive`.
    pub fn rate_metadata(&self) -> u8 {
        (self.words[0] & 3) as u8
    }

    /// Raw ranging time extracted by `fsldrv_gpon_ploamu_receive`:
    /// `((words[0] >> 5) & 0x1f_ffff) + 0x30`.
    pub fn ranging_time_raw(&self) -> u32 {
        ((self.words[0] >> 5) & 0x1f_ffff) + 0x30
    }

    /// Protocol ONU-ID byte from the 13-byte GPON PLOAM frame (`0xff` is unassigned/invalid).
    pub fn onu_id(&self) -> Option<u8> {
        (self.frame[0] != 0xff).then_some(self.frame[0])
    }

    /// Protocol message-ID byte from the 13-byte GPON PLOAM frame.
    pub fn message_id(&self) -> u8 {
        self.frame[1]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmciKind {
    Omci,
    Wmci,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmciFrame {
    pub metadata: u32,
    /// Raw header bits21:14 passed by the stock stack to its event function.
    /// No ONU-ID, registration-generation or GEM mapping is inferred here.
    pub source: u8,
    pub kind: OmciKind,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceivedFrame {
    Ploam(PloamRecord),
    Omci(OmciFrame),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumption {
    /// No consume write was attempted by this call. A complete read may still
    /// be retained in the error; do not deliver it and then receive it again.
    NotAttempted,
    /// Consume was attempted or the session was already ambiguous. Reconcile
    /// before further access; an ioctl error cannot tell whether it took effect.
    Indeterminate,
}

#[derive(Debug)]
pub struct ReceiveError {
    pub consumption: Consumption,
    /// A completely read frame is retained if cancellation/consume fails.
    /// Its presence does not grant permission to replay or dispatch it twice.
    pub frame: Option<ReceivedFrame>,
    pub source: io::Error,
}

impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FSL management RX {:?}: {}",
            self.consumption, self.source
        )
    }
}

impl std::error::Error for ReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl<B: Bus> Management<B> {
    /// Returns at most one record. Empty polls never clear valid/last. IRQ mask
    /// handling belongs to the future runtime, not this polling transaction.
    pub fn receive_ploam(
        &mut self,
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> Result<Option<PloamRecord>, ReceiveError> {
        let mut retained = None;
        let result = (|| {
            self.ensure_ready()?;
            for attempt in 0..policy.max_reads.get() {
                checkpoint(if attempt == 0 {
                    Duration::ZERO
                } else {
                    policy.interval
                })?;
                let valid = self.fsl.read_register(PLOAM_VALID)? & 1 != 0;
                let last = self.read(PLOAM_LAST, checkpoint)? & 1 != 0;
                if !valid {
                    continue;
                }
                checkpoint(Duration::ZERO)?;
                let words = self.fsl.read(RegisterSpace::Register, PLOAM_DATA, 0, 13)?;
                let record = PloamRecord::decode(words.try_into().unwrap(), last);
                retained = Some(ReceivedFrame::Ploam(record.clone()));
                self.submit(PLOAM_VALID, 0, checkpoint)?;
                if last {
                    self.submit(PLOAM_LAST, 0, checkpoint)?;
                }
                self.indeterminate = false;
                return Ok(Some(record));
            }
            Ok(None)
        })();
        self.finish_receive(result, retained)
    }

    /// Returns one opaque frame after consumption, without interpreting OMCI
    /// CRC/framing, mapping an ONU, or claiming optical-link liveness.
    pub fn receive_omci_frame(
        &mut self,
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> Result<Option<OmciFrame>, ReceiveError> {
        let mut retained = None;
        let result = (|| {
            self.ensure_ready()?;
            let mut row = self.omci_cursor.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FSL OMCI ring cursor is not synchronized",
                )
            })?;
            if !self.omci_available(policy, checkpoint)? {
                return Ok(None);
            }
            let marker = self.read(OMCI_MARKER, checkpoint)?;
            // Stock scans without a bound. Never inspect the same ring row twice
            // looking for a header, and do not commit skipped rows on failure.
            for _ in 0..OMCI_RING_ROWS {
                let header = self.read_omci_row(row, checkpoint)?;
                if header[1] != marker {
                    row = row.wrapping_add(1);
                    continue;
                }
                let metadata = header[0];
                let len = (metadata & 0x3fff) as usize;
                if !(1..=MAX_OMCI_BYTES).contains(&len) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid FSL upstream frame length",
                    ));
                }
                let mut payload = Vec::with_capacity(len);
                for _ in 0..len.div_ceil(8) {
                    row = row.wrapping_add(1);
                    let words = self.read_omci_row(row, checkpoint)?;
                    // Logical low/high words: the SPI layer reverses memory
                    // word order, yielding the original eight software bytes.
                    let mut bytes = [0; 8];
                    bytes[..4].copy_from_slice(&words[1].to_be_bytes());
                    bytes[4..].copy_from_slice(&words[0].to_be_bytes());
                    payload.extend_from_slice(&bytes[..(len - payload.len()).min(8)]);
                }
                let frame = OmciFrame {
                    metadata,
                    source: ((metadata >> 14) & 0xff) as u8,
                    kind: if metadata & (1 << 31) != 0 {
                        OmciKind::Omci
                    } else {
                        OmciKind::Wmci
                    },
                    payload,
                };
                retained = Some(ReceivedFrame::Omci(frame.clone()));
                // Descriptor defines only bit0 as RD_OVER_PLS. Do not copy the
                // stock 0xf write into reserved bits, or RMW a pulse register.
                self.submit(OMCI_CONSUME, 1, checkpoint)?;
                self.omci_cursor = Some(row.wrapping_add(1));
                self.indeterminate = false;
                return Ok(Some(frame));
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "FSL upstream marker absent from bounded ring scan",
            ))
        })();
        self.finish_receive(result, retained)
    }

    fn omci_available(
        &mut self,
        policy: PollPolicy,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<bool> {
        for attempt in 0..policy.max_reads.get() {
            checkpoint(if attempt == 0 {
                Duration::ZERO
            } else {
                policy.interval
            })?;
            if self.fsl.read_register(OMCI_STATUS)? & 1 != 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn read_omci_row(
        &mut self,
        row: u8,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<[u32; 2]> {
        checkpoint(Duration::ZERO)?;
        Ok(self
            .fsl
            .read(RegisterSpace::Memory, OMCI_MEMORY, u32::from(row), 2)?
            .try_into()
            .unwrap())
    }

    fn finish_receive<T>(
        &self,
        result: io::Result<T>,
        frame: Option<ReceivedFrame>,
    ) -> Result<T, ReceiveError> {
        result.map_err(|source| ReceiveError {
            consumption: if self.indeterminate {
                Consumption::Indeterminate
            } else {
                Consumption::NotAttempted
            },
            frame,
            source,
        })
    }
}

#[cfg(test)]
mod tests;
