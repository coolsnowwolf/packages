//! Stock board-helper I²C message shapes, with explicit adapter ownership.
//! No bus discovery, chip probing, calibration writes or automatic readback.
use std::io;

pub mod i2c_dev;

pub const MAX_DATA_BYTES: usize = 16;

/// One combined Linux-style transfer: one write message, followed by a read
/// message iff `read` is nonempty. Returns the completed message count. Do not
/// replay failed operations in the implementation; adapters may retry arbitration
/// internally. The caller owns the adapter, page state and common optical lease.
pub trait Transfer {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> io::Result<usize>;
}

pub struct OpticalBus<T> {
    transport: T,
    indeterminate: bool,
}

impl<T: Transfer> OpticalBus<T> {
    /// The transport must already refer to the independently verified downstream
    /// adapter. This does not identify a bus or initialize the optical hardware.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            indeterminate: false,
        }
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.indeterminate
    }

    pub(super) fn require_reconciliation(&mut self) {
        self.indeterminate = true;
    }

    /// Repeated START between the one-byte register pointer and read message.
    /// Failed/short transfers leave the caller's output unchanged.
    pub fn read(&mut self, address: u8, register: u8, output: &mut [u8]) -> io::Result<()> {
        self.validate(address, output.len())?;
        let mut scratch = [0; MAX_DATA_BYTES];
        let count = self
            .transport
            .transfer(address, &[register], &mut scratch[..output.len()])?;
        complete(count, 2)?;
        output.copy_from_slice(&scratch[..output.len()]);
        Ok(())
    }

    /// A single message containing the register byte and data. No implicit read,
    /// splitting or retry. Any failed attempt may have changed page/calibration
    /// state and freezes this session until external reconciliation/reinitialization.
    pub fn write(&mut self, address: u8, register: u8, data: &[u8]) -> io::Result<()> {
        self.validate(address, data.len())?;
        let mut message = [0; MAX_DATA_BYTES + 1];
        message[0] = register;
        message[1..=data.len()].copy_from_slice(data);
        self.indeterminate = true;
        let count = self
            .transport
            .transfer(address, &message[..=data.len()], &mut [])?;
        complete(count, 1)?;
        self.indeterminate = false;
        Ok(())
    }

    fn validate(&self, address: u8, len: usize) -> io::Result<()> {
        if self.indeterminate {
            return Err(io::Error::other("optical write needs reconciliation"));
        }
        if address > 0x7f || !(1..=MAX_DATA_BYTES).contains(&len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid optical I2C address/length",
            ));
        }
        Ok(())
    }
}

fn complete(actual: usize, expected: usize) -> io::Result<()> {
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete optical I2C transfer",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
