//! AN4475-G2's persisted-table-selected UX3326S optical profile.
//!
//! The profile is gated by an independent live signature read and the validated
//! calibration table discriminator. It does not probe other chip families.
use std::io;

use super::{calibration::CalibrationTable, optical::OpticalBus, optical::Transfer};

pub const UX3326S_CHIP_ID: u8 = 0x27;
pub const UX3326S_DEVICE_ADDRESS: u8 = 0x63;
pub const UX3326S_SIGNATURE_REGISTER: u8 = 0xc4;
pub const UX3326S_PAGE_REGISTER: u8 = 0x7f;
pub const UX3326S_PROFILE_BYTES: usize = 0x400;
pub const UX3326S_REGISTER_WRITES: u32 = 802;
/// The stock helper performs this many busy-loop iterations after every write.
/// It is an iteration count, not a portable time duration.
pub const STOCK_DELAY_ITERATIONS: u32 = 0x320 * 0xa7;

const UX3326S_SIGNATURE: &[u8; 8] = b"UX3326S1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentifiedOpticalChip {
    discriminator: u8,
    page: u8,
}

impl IdentifiedOpticalChip {
    pub fn discriminator(self) -> u8 {
        self.discriminator
    }

    pub fn current_page(self) -> u8 {
        self.page
    }
}

/// Reproduces the original hwcfg-selected UX3326S identity transaction:
/// read register 0, select page 0x80, then read the eight-byte signature at 0xc4.
/// The page write is a side effect and is deliberately not undone on mismatch.
pub fn identify_ux3326s<T: Transfer>(bus: &mut OpticalBus<T>) -> io::Result<IdentifiedOpticalChip> {
    let mut probe = [0; 1];
    bus.read(UX3326S_DEVICE_ADDRESS, 0x00, &mut probe)?;

    bus.write(UX3326S_DEVICE_ADDRESS, UX3326S_PAGE_REGISTER, &[0x80])?;

    let mut signature = [0; 8];
    bus.read(
        UX3326S_DEVICE_ADDRESS,
        UX3326S_SIGNATURE_REGISTER,
        &mut signature,
    )?;
    if &signature != UX3326S_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "downstream optical chip is not UX3326S1",
        ));
    }

    Ok(IdentifiedOpticalChip {
        discriminator: UX3326S_CHIP_ID,
        page: 0x80,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterWrite {
    pub register: u8,
    pub value: u8,
}

/// The caller must supply a target-specific implementation. There is no
/// portable conversion from the vendor's CPU busy-loop count to elapsed time.
pub trait StockPacer {
    fn after_write(&mut self, loop_iterations: u32) -> io::Result<()>;
}

/// Apply only the observed UX3326S profile. Each register write is one checked
/// I²C transaction; any error stops the sequence without retry or readback.
/// Cancellation is checked before every write and pacing interval.
pub fn program_ux3326s<T, P, C>(
    bus: &mut OpticalBus<T>,
    chip: IdentifiedOpticalChip,
    table: &CalibrationTable<'_>,
    pacer: &mut P,
    mut checkpoint: C,
) -> io::Result<()>
where
    T: Transfer,
    P: StockPacer,
    C: FnMut() -> io::Result<()>,
{
    if chip.discriminator != UX3326S_CHIP_ID || chip.page != 0x80 {
        return Err(invalid(
            "UX3326S identity/page precondition is not satisfied",
        ));
    }
    table.require_chip(chip.discriminator)?;

    let mut wrote_any = false;
    for write in ux3326s_writes(table.payload())? {
        if let Err(error) = checkpoint() {
            if wrote_any {
                bus.require_reconciliation();
            }
            return Err(error);
        }
        bus.write(UX3326S_DEVICE_ADDRESS, write.register, &[write.value])?;
        wrote_any = true;
        if let Err(error) = checkpoint().and_then(|()| pacer.after_write(STOCK_DELAY_ITERATIONS)) {
            bus.require_reconciliation();
            return Err(error);
        }
    }
    debug_assert!(wrote_any);
    Ok(())
}

/// Validate the selected persisted profile against the live signature, then
/// apply the matching register schedule. The caller still owns table loading,
/// provenance checks, the optical lease, and a target-specific stock pacer.
pub fn initialize_ux3326s<T, P, C>(
    bus: &mut OpticalBus<T>,
    table: &CalibrationTable<'_>,
    pacer: &mut P,
    checkpoint: C,
) -> io::Result<IdentifiedOpticalChip>
where
    T: Transfer,
    P: StockPacer,
    C: FnMut() -> io::Result<()>,
{
    if table.chip_id() != UX3326S_CHIP_ID {
        return Err(invalid("persisted optical profile is not UX3326S"));
    }
    let chip = identify_ux3326s(bus)?;
    program_ux3326s(bus, chip, table, pacer, checkpoint)?;
    Ok(chip)
}

/// Expand the exact stock write schedule, excluding delay emulation.
pub fn ux3326s_writes(payload: &[u8]) -> io::Result<Vec<RegisterWrite>> {
    if payload.len() != super::calibration::PAYLOAD_BYTES {
        return Err(invalid("UX3326S calibration payload has the wrong length"));
    }
    let mut writes = Vec::with_capacity(802);

    for (offset, value) in (0x7b_u8..=0x7e).zip([0x66, 0x99, 0x55, 0xaa]) {
        writes.push(RegisterWrite {
            register: offset,
            value,
        });
    }

    append_chunk(&mut writes, payload, 0x000, 0x000, 0x04c)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x80,
    });
    append_chunk(&mut writes, payload, 0x100, 0x080, 0x04c)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x81,
    });
    append_chunk(&mut writes, payload, 0x180, 0x080, 0x080)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x82,
    });
    append_chunk(&mut writes, payload, 0x200, 0x080, 0x080)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x83,
    });
    append_chunk(&mut writes, payload, 0x280, 0x080, 0x080)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x84,
    });
    append_chunk(&mut writes, payload, 0x300, 0x080, 0x080)?;
    writes.push(RegisterWrite {
        register: UX3326S_PAGE_REGISTER,
        value: 0x85,
    });
    append_chunk(&mut writes, payload, 0x380, 0x080, 0x080)?;

    debug_assert_eq!(writes.len(), 802);
    Ok(writes)
}

fn append_chunk(
    writes: &mut Vec<RegisterWrite>,
    payload: &[u8],
    source_offset: usize,
    register_start: usize,
    len: usize,
) -> io::Result<()> {
    let bytes = payload
        .get(source_offset..source_offset + len)
        .ok_or_else(|| invalid("UX3326S profile range is out of bounds"))?;
    for (offset, value) in bytes.iter().copied().enumerate() {
        writes.push(RegisterWrite {
            register: register_start.wrapping_add(offset) as u8,
            value,
        });
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
