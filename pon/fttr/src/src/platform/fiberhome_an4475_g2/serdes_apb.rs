//! Bounded access to the chip's indirect SerDes APB window.
//!
//! This is a low-level primitive only. It does not configure clocks, reset,
//! mode, or the OLT/NNI SerDes profiles.

use super::protocol::{Bus, Fsl};
use std::{io, time::Duration};

const BUS_SELECT: u32 = 0x1100;
const APB_WDATA: u32 = 0x1101;
const APB_RDATA: u32 = 0x1102;
const APB_CTRL: u32 = 0x1103;
const APB_ADDRESS_MASK: u32 = 0xffff;
const APB_WRITE: u32 = 1 << 16;
const APB_ACK: u32 = 1 << 17;
const APB_ERROR: u32 = 1 << 18;

/// The stock code performs at most 100 ACK reads with a 1 us delay between
/// reads. The production caller supplies the actual wait/cancellation policy.
pub const APB_ACK_READS: u16 = 100;
pub const APB_ACK_INTERVAL: Duration = Duration::from_micros(1);

/// Owns one FSL session because APB selector and command registers are shared.
/// A failed command or exhausted ACK budget leaves the indirect transaction
/// state unknown and permanently blocks this session; no command is replayed.
pub struct SerdesApb<B> {
    fsl: Fsl<B>,
    bus: u8,
    indeterminate: bool,
}

impl<B: Bus> SerdesApb<B> {
    pub fn new(fsl: Fsl<B>, bus: u8) -> io::Result<Self> {
        if bus > 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SerDes APB bus selector must fit the two-bit field",
            ));
        }
        Ok(Self {
            fsl,
            bus,
            indeterminate: false,
        })
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.indeterminate
    }

    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn set_bus(&mut self, bus: u8) -> io::Result<()> {
        self.ensure_ready()?;
        validate_bus(bus)?;
        self.bus = bus;
        Ok(())
    }

    pub fn into_inner(self) -> Fsl<B> {
        self.fsl
    }

    pub fn read(
        &mut self,
        address: u16,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<u8> {
        self.ensure_ready()?;
        checkpoint(Duration::ZERO)?;
        self.indeterminate = true;
        let value = read_on_unchecked(&mut self.fsl, self.bus, address, checkpoint)?;
        self.indeterminate = false;
        Ok(value)
    }

    pub fn write(
        &mut self,
        address: u16,
        value: u8,
        checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> io::Result<()> {
        self.ensure_ready()?;
        checkpoint(Duration::ZERO)?;
        self.indeterminate = true;
        write_on_unchecked(&mut self.fsl, self.bus, address, value, checkpoint)?;
        self.indeterminate = false;
        Ok(())
    }

    fn ensure_ready(&self) -> io::Result<()> {
        if self.indeterminate {
            return Err(io::Error::other(
                "SerDes APB session requires reconciliation",
            ));
        }
        Ok(())
    }
}

fn validate_bus(bus: u8) -> io::Result<()> {
    if bus > 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SerDes APB bus selector must fit the two-bit field",
        ));
    }
    Ok(())
}

pub(crate) fn read_on<B: Bus>(
    fsl: &mut Fsl<B>,
    bus: u8,
    address: u16,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u8> {
    validate_bus(bus)?;
    checkpoint(Duration::ZERO)?;
    read_on_unchecked(fsl, bus, address, checkpoint)
}

pub(crate) fn write_on<B: Bus>(
    fsl: &mut Fsl<B>,
    bus: u8,
    address: u16,
    value: u8,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    validate_bus(bus)?;
    checkpoint(Duration::ZERO)?;
    write_on_unchecked(fsl, bus, address, value, checkpoint)
}

fn read_on_unchecked<B: Bus>(
    fsl: &mut Fsl<B>,
    bus: u8,
    address: u16,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u8> {
    write_register(fsl, BUS_SELECT, bus as u32)?;
    checkpoint(Duration::ZERO)?;
    let ctrl = fsl.read_register(APB_CTRL)?;
    let command = (ctrl & !(APB_ADDRESS_MASK | APB_WRITE | APB_ACK | APB_ERROR)) | address as u32;
    write_register(fsl, APB_CTRL, command)?;
    let status = wait_ack(fsl, checkpoint)?;
    if status & APB_ERROR != 0 {
        return Err(io::Error::other("SerDes APB target reported an error"));
    }
    checkpoint(Duration::ZERO)?;
    Ok(fsl.read_register(APB_RDATA)? as u8)
}

fn write_on_unchecked<B: Bus>(
    fsl: &mut Fsl<B>,
    bus: u8,
    address: u16,
    value: u8,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    write_register(fsl, BUS_SELECT, bus as u32)?;
    checkpoint(Duration::ZERO)?;
    write_register(fsl, APB_WDATA, value as u32)?;
    checkpoint(Duration::ZERO)?;
    let ctrl = fsl.read_register(APB_CTRL)?;
    let command =
        (ctrl & !(APB_ADDRESS_MASK | APB_WRITE | APB_ACK | APB_ERROR)) | APB_WRITE | address as u32;
    write_register(fsl, APB_CTRL, command)?;
    let status = wait_ack(fsl, checkpoint)?;
    if status & APB_ERROR != 0 {
        return Err(io::Error::other("SerDes APB target reported an error"));
    }
    Ok(())
}

fn write_register<B: Bus>(fsl: &mut Fsl<B>, address: u32, value: u32) -> io::Result<()> {
    fsl.write(
        super::super::diagnostics::RegisterSpace::Register,
        address,
        0,
        &[value],
    )
}

fn wait_ack<B: Bus>(
    fsl: &mut Fsl<B>,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u32> {
    for attempt in 0..APB_ACK_READS {
        checkpoint(if attempt == 0 {
            Duration::ZERO
        } else {
            APB_ACK_INTERVAL
        })?;
        let status = fsl.read_register(APB_CTRL)?;
        if status & APB_ACK != 0 {
            return Ok(status);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "SerDes APB ACK polling budget exhausted",
    ))
}

#[cfg(test)]
mod tests;
