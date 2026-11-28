//! Stock GPON interrupt status, mask, clear, and poll-dispatch primitives.
//!
//! Recovered from `fsl_core_fttr_chip.ko` (`fsldrv_gpon_interrupt_get`,
//! `fsldrv_gpon_interrupt_mask_get`, `fsldrv_gpon_interrupt_mask_config`,
//! `fsldrv_gpon_interrupt_clear`, `fslstack_gpon_interrupt_dispatch`, and
//! `fslstack_gpon_global_init`). Note that the stock stack polls this dispatch
//! routine every 20 ms rather than using a hardware IRQ line.

use super::{
    protocol::{Bus, Fsl},
    upstream::PLOAM_VALID,
};
use crate::platform::diagnostics::RegisterSpace;
use std::io;

/// `GPON_INT_MASK` (reg ID `0xde` / 222, 7-bit, base address `0x210`).
pub const GPON_INT_MASK: u32 = 0x210;
/// `GPON_DYING_GASP_ALM` (reg ID `0xd2` / 210, 128-bit / 4 words, base address `0x265`).
pub const GPON_DYING_GASP_ALM: u32 = 0x265;
/// `GPON_LOSI_ALM` (reg ID `0xd3` / 211, 128-bit / 4 words, base address `0x269`).
pub const GPON_LOSI_ALM: u32 = 0x269;
/// `GPON_LOFI_ALM` (reg ID `0xd4` / 212, 128-bit / 4 words, base address `0x26d`).
pub const GPON_LOFI_ALM: u32 = 0x26d;
/// `GPON_INT` (reg ID `0xdf` / 223, 7-bit W1C status, base address `0x2f0`).
pub const GPON_INT: u32 = 0x2f0;
/// `PLOAM_RD_VLD_CFG` (reg ID `0x137` / 311, 1-bit, base address `0x41b`).
pub const PLOAM_RD_VLD_CFG: u32 = PLOAM_VALID;
/// `PLOAM_RX_INT_MASK` (reg ID `0x155` / 341, 1-bit, base address `0x44a`).
pub const PLOAM_RX_INT_MASK: u32 = 0x44a;
/// `PLOAM_LOAMI_SUFI_INT_MASK` (reg ID `0x156` / 342, 2-bit, base address `0x44b`).
pub const PLOAM_LOAMI_SUFI_INT_MASK: u32 = 0x44b;
/// `PLOAM_LOAMI_SUFI_INT` (reg ID `0x157` / 343, 2-bit W1C status, base address `0x49d`).
pub const PLOAM_LOAMI_SUFI_INT: u32 = 0x49d;
/// `PLOAM_RX_INT` (reg ID `0x158` / 344, 1-bit W1C status, base address `0x49e`).
pub const PLOAM_RX_INT: u32 = 0x49e;
/// `NNI_INT_MASK` (reg ID `0x10b` / 267, 1-bit, base address `0x915`).
pub const NNI_INT_MASK: u32 = 0x915;
/// `NNI_INT` (reg ID `0x10c` / 268, 1-bit W1C status, base address `0x922`).
pub const NNI_INT: u32 = 0x922;
/// `SOFT_RDWR_OMCI_STA` (reg ID `0x107` / 263, 2-bit RO status, base address `0x924`).
pub const SOFT_RDWR_OMCI_STA: u32 = 0x924;

const BIT_TIWI: u32 = 1 << 0;
const BIT_DOWI: u32 = 1 << 1;
const BIT_LOFI: u32 = 1 << 2;
const BIT_LOSI: u32 = 1 << 3;
const BIT_RSSI: u32 = 1 << 4;
const BIT_DYING_GASP: u32 = 1 << 5;

const BIT_LOAMI: u32 = 1 << 0;
const BIT_SUFI: u32 = 1 << 1;

const BIT_PLOAM_RD: u32 = 1 << 0;
const BIT_OMCI_RX: u32 = 1 << 0;

/// GPON hardware interrupt sources modeled by `fsldrv_gpon_interrupt_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GponInterrupt {
    PloamRx,
    LoAmi,
    SuF,
    DyingGasp,
    Rssi,
    Losi,
    Lofi,
    Dowi,
    Tiwi,
    OmciRx,
}

impl GponInterrupt {
    /// Dispatch order verified in `fslstack_gpon_interrupt_dispatch`.
    pub const DISPATCH_ORDER: [Self; 9] = [
        Self::PloamRx,
        Self::Losi,
        Self::Lofi,
        Self::Dowi,
        Self::Tiwi,
        Self::LoAmi,
        Self::SuF,
        Self::DyingGasp,
        Self::OmciRx,
    ];

    /// Stock numeric interrupt ID passed to `fsldrv_gpon_interrupt_mask_config`
    /// and `fsldrv_gpon_interrupt_clear`.
    pub const fn stock_code(self) -> u8 {
        match self {
            Self::PloamRx => 3,
            Self::LoAmi => 4,
            Self::SuF => 5,
            Self::DyingGasp => 7,
            Self::Rssi => 8,
            Self::Losi => 9,
            Self::Lofi => 10,
            Self::Dowi => 11,
            Self::Tiwi => 12,
            Self::OmciRx => 13,
        }
    }
}

/// Decoded 10-element interrupt status or mask vector returned by
/// `fsldrv_gpon_interrupt_get` / `fsldrv_gpon_interrupt_mask_get`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GponInterruptFlags {
    pub tiwi: bool,
    pub dowi: bool,
    pub lofi: bool,
    pub losi: bool,
    pub rssi: bool,
    pub dying_gasp: bool,
    pub sufi: bool,
    pub loami: bool,
    pub ploam_rx: bool,
    pub omci_rx: bool,
}

impl GponInterruptFlags {
    pub fn is_set(self, kind: GponInterrupt) -> bool {
        match kind {
            GponInterrupt::Tiwi => self.tiwi,
            GponInterrupt::Dowi => self.dowi,
            GponInterrupt::Lofi => self.lofi,
            GponInterrupt::Losi => self.losi,
            GponInterrupt::Rssi => self.rssi,
            GponInterrupt::DyingGasp => self.dying_gasp,
            GponInterrupt::SuF => self.sufi,
            GponInterrupt::LoAmi => self.loami,
            GponInterrupt::PloamRx => self.ploam_rx,
            GponInterrupt::OmciRx => self.omci_rx,
        }
    }

    pub fn set(&mut self, kind: GponInterrupt, value: bool) {
        match kind {
            GponInterrupt::Tiwi => self.tiwi = value,
            GponInterrupt::Dowi => self.dowi = value,
            GponInterrupt::Lofi => self.lofi = value,
            GponInterrupt::Losi => self.losi = value,
            GponInterrupt::Rssi => self.rssi = value,
            GponInterrupt::DyingGasp => self.dying_gasp = value,
            GponInterrupt::SuF => self.sufi = value,
            GponInterrupt::LoAmi => self.loami = value,
            GponInterrupt::PloamRx => self.ploam_rx = value,
            GponInterrupt::OmciRx => self.omci_rx = value,
        }
    }

    /// Returns active interrupts allowed by `mask` in stock dispatch order.
    pub fn dispatch_list(self, mask: Self) -> Vec<GponInterrupt> {
        GponInterrupt::DISPATCH_ORDER
            .into_iter()
            .filter(|&kind| self.is_set(kind) && mask.is_set(kind))
            .collect()
    }
}

fn decode_gpon_int_word(raw: u32, dst: &mut GponInterruptFlags) {
    // Stock `fsldrv_gpon_interrupt_get` clamps 0xffffffff to 0 on `GPON_INT`.
    let word = if raw == u32::MAX { 0 } else { raw };
    dst.tiwi = word & BIT_TIWI != 0;
    dst.dowi = word & BIT_DOWI != 0;
    dst.lofi = word & BIT_LOFI != 0;
    dst.losi = word & BIT_LOSI != 0;
    dst.rssi = word & BIT_RSSI != 0;
    dst.dying_gasp = word & BIT_DYING_GASP != 0;
}

/// Reads GPON interrupt status (`fsldrv_gpon_interrupt_get`).
pub fn read_status<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<GponInterruptFlags> {
    let mut flags = GponInterruptFlags::default();
    let gpon_int = fsl.read_register(GPON_INT)?;
    decode_gpon_int_word(gpon_int, &mut flags);

    let loami_sufi = fsl.read_register(PLOAM_LOAMI_SUFI_INT)?;
    flags.sufi = loami_sufi & BIT_SUFI != 0;
    flags.loami = loami_sufi & BIT_LOAMI != 0;

    let ploam_rx = fsl.read_register(PLOAM_RX_INT)?;
    let ploam_rx = if ploam_rx == u32::MAX { 0 } else { ploam_rx };
    flags.ploam_rx = ploam_rx & BIT_PLOAM_RD != 0;
    if fsl.read_register(PLOAM_RD_VLD_CFG)? != 0 {
        flags.ploam_rx = true;
    }

    let omci_sta = fsl.read_register(SOFT_RDWR_OMCI_STA)?;
    flags.omci_rx = omci_sta & BIT_OMCI_RX != 0;
    Ok(flags)
}

/// Reads GPON interrupt enable masks (`fsldrv_gpon_interrupt_mask_get`).
pub fn read_mask<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<GponInterruptFlags> {
    let mut flags = GponInterruptFlags::default();
    let gpon_mask = fsl.read_register(GPON_INT_MASK)?;
    flags.tiwi = gpon_mask & BIT_TIWI != 0;
    flags.dowi = gpon_mask & BIT_DOWI != 0;
    flags.lofi = gpon_mask & BIT_LOFI != 0;
    flags.losi = gpon_mask & BIT_LOSI != 0;
    flags.rssi = gpon_mask & BIT_RSSI != 0;
    flags.dying_gasp = gpon_mask & BIT_DYING_GASP != 0;

    let loami_sufi = fsl.read_register(PLOAM_LOAMI_SUFI_INT_MASK)?;
    flags.sufi = loami_sufi & BIT_SUFI != 0;
    flags.loami = loami_sufi & BIT_LOAMI != 0;

    let ploam_mask = fsl.read_register(PLOAM_RX_INT_MASK)?;
    flags.ploam_rx = ploam_mask & BIT_PLOAM_RD != 0;
    if fsl.read_register(PLOAM_RD_VLD_CFG)? != 0 {
        flags.ploam_rx = true;
    }

    let nni_mask = fsl.read_register(NNI_INT_MASK)?;
    flags.omci_rx = nni_mask & BIT_OMCI_RX != 0;
    Ok(flags)
}

fn mask_target(kind: GponInterrupt) -> (u32, u32) {
    match kind {
        GponInterrupt::PloamRx => (PLOAM_RX_INT_MASK, BIT_PLOAM_RD),
        GponInterrupt::LoAmi => (PLOAM_LOAMI_SUFI_INT_MASK, BIT_LOAMI),
        GponInterrupt::SuF => (PLOAM_LOAMI_SUFI_INT_MASK, BIT_SUFI),
        GponInterrupt::DyingGasp => (GPON_INT_MASK, BIT_DYING_GASP),
        GponInterrupt::Rssi => (GPON_INT_MASK, BIT_RSSI),
        GponInterrupt::Losi => (GPON_INT_MASK, BIT_LOSI),
        GponInterrupt::Lofi => (GPON_INT_MASK, BIT_LOFI),
        GponInterrupt::Dowi => (GPON_INT_MASK, BIT_DOWI),
        GponInterrupt::Tiwi => (GPON_INT_MASK, BIT_TIWI),
        GponInterrupt::OmciRx => (NNI_INT_MASK, BIT_OMCI_RX),
    }
}

/// Enables or disables one interrupt mask bit (`fsldrv_gpon_interrupt_mask_config`).
pub fn set_mask<B: Bus>(fsl: &mut Fsl<B>, kind: GponInterrupt, enable: bool) -> io::Result<()> {
    let (addr, bit) = mask_target(kind);
    let mut reg = fsl.read_register(addr)?;
    if enable {
        reg |= bit;
    } else {
        reg &= !bit;
    }
    fsl.write(RegisterSpace::Register, addr, 0, &[reg])
}

/// Sets or clears all bits of `GPON_INT_MASK` (`0x210`)
/// (`fsldrv_gpon_interrupt_mask_config` case 14).
pub fn set_all_gpon_int_masks<B: Bus>(fsl: &mut Fsl<B>, enable: bool) -> io::Result<()> {
    let val = if enable { u32::MAX } else { 0 };
    fsl.write(RegisterSpace::Register, GPON_INT_MASK, 0, &[val])
}

/// Clears one latched W1C interrupt status bit (`fsldrv_gpon_interrupt_clear`).
pub fn clear_interrupt<B: Bus>(fsl: &mut Fsl<B>, kind: GponInterrupt) -> io::Result<()> {
    let (addr, bit) = match kind {
        GponInterrupt::PloamRx => (PLOAM_RX_INT, BIT_PLOAM_RD),
        GponInterrupt::LoAmi => (PLOAM_LOAMI_SUFI_INT, BIT_LOAMI),
        GponInterrupt::SuF => (PLOAM_LOAMI_SUFI_INT, BIT_SUFI),
        GponInterrupt::DyingGasp => (GPON_INT, BIT_DYING_GASP),
        GponInterrupt::Rssi => (GPON_INT, BIT_RSSI),
        GponInterrupt::Losi => (GPON_INT, BIT_LOSI),
        GponInterrupt::Lofi => (GPON_INT, BIT_LOFI),
        GponInterrupt::Dowi => (GPON_INT, BIT_DOWI),
        GponInterrupt::Tiwi => (GPON_INT, BIT_TIWI),
        GponInterrupt::OmciRx => (NNI_INT, BIT_OMCI_RX),
    };
    fsl.write(RegisterSpace::Register, addr, 0, &[bit])
}

/// Polls status and mask registers, clears every unmasked pending interrupt in
/// `fslstack_gpon_interrupt_dispatch` priority order, and returns the cleared events.
pub fn poll_and_clear_pending<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<Vec<GponInterrupt>> {
    let status = read_status(fsl)?;
    let mask = read_mask(fsl)?;
    let pending = status.dispatch_list(mask);
    for &kind in &pending {
        clear_interrupt(fsl, kind)?;
    }
    Ok(pending)
}

/// Reads one 128-bit per-ONU alarm bitmap register
/// (`GPON_DYING_GASP_ALM`, `GPON_LOSI_ALM`, or `GPON_LOFI_ALM`).
///
/// TODO(bench): the hardware latch/clear semantics of these bitmaps are
/// unverified. The stock LOFI handler reads its bitmap for deactivation,
/// reads it again via `fsldrv_gpon_alarm_read_clear`, then clears the W1C
/// interrupt. The function name alone does not establish read-clear semantics.
/// This implementation clears the interrupt before reading the bitmap and
/// never writes the bitmap back. Compare bounded, consecutive reads and
/// interrupt timing on hardware; repeated or cleared bits alone cannot
/// distinguish a latched event from a continuing or renewed condition.
pub fn read_alarm_bitmap<B: Bus>(fsl: &mut Fsl<B>, reg_addr: u32) -> io::Result<[u32; 4]> {
    if !matches!(
        reg_addr,
        GPON_DYING_GASP_ALM | GPON_LOSI_ALM | GPON_LOFI_ALM
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported per-ONU alarm bitmap register",
        ));
    }
    let mut words = [0; 4];
    fsl.read_into(RegisterSpace::Register, reg_addr, 0, &mut words)?;
    Ok(words)
}

/// Returns whether `onu_index` (`0..128`) is set in a 128-bit alarm bitmap.
pub fn alarm_bitmap_contains(words: &[u32; 4], onu_index: u16) -> bool {
    if onu_index >= 128 {
        return false;
    }
    let word = words[usize::from(onu_index >> 5)];
    (word >> (onu_index & 0x1f)) & 1 != 0
}

#[cfg(test)]
mod tests;
