//! Static AN4475-G2 / FSL hardware bring-up and GPON default register
//! configuration sequences extracted from `fsl_core_fttr_chip.ko`.
//!
//! These primitives implement the verified register, reset, PLL, SerDes APB,
//! MAU, and RAM-init sequences behind bounded checkpoints and poll policies.
//! Board-specific SPI bus/CS/clock parameters and live SerDes/optical bring-up
//! still require target validation before wiring into a runtime backend.

pub mod defaults;
pub mod serdes_profiles;

use super::{
    gdba::GdbaScene,
    polling::{self, PollPolicy},
    protocol::{Bus, Fsl},
    serdes_apb,
};
use crate::platform::diagnostics::RegisterSpace;
use serdes_profiles::{
    efuse_0x30_transform, ETH_EFUSE_ADDRESS, ETH_SERDES_1P25G_PATCHES, ETH_SERDES_3P125G_WRITES,
    ETH_SERDES_CLK_MODE1_VALUE, ETH_SERDES_CLK_PATCH_INDEX, PON_SERDES_2P5G_WRITES,
    PON_SERDES_CLK_MODE1_VALUE, PON_SERDES_CLK_PATCH_INDEX,
};
use std::{io, num::NonZeroU16, time::Duration};

pub const RAM_INIT_STATUS: u32 = 0x31;
pub const GPON_RAM_INIT_MASK: u32 = 0xbf;

pub const PON_TYPE_CFG: u32 = 0x1;
pub const ETH_TYPE_CFG: u32 = 0x2;
pub const SPI_MISO_CFG: u32 = 0x4;
pub const MUX_SEL_CFG: u32 = 0xc;
pub const TOD_LOGIC_RST: u32 = 0xf;
pub const GLOBAL_RST_LOGIC: u32 = 0x10;
pub const GLOBAL_RST_FUCI: u32 = 0x11;
pub const PON_SYS_LOGIC_RST: u32 = 0x12;
pub const PON_SERDES_US_LOGIC_RST: u32 = 0x13;
pub const PON_SERDES_DS_LOGIC_RST: u32 = 0x14;
pub const ETH_SYS_LOGIC_RST: u32 = 0x15;
pub const ETH_SERDES_RX_LOGIC_RST: u32 = 0x16;
pub const ETH_SERDES_TX_LOGIC_RST: u32 = 0x17;
pub const ETH_SERDES_RX_DIV_LOGIC_RST: u32 = 0x18;
pub const PON_SYS_FUCI_RST: u32 = 0x19;
pub const ETH_SYS_FUCI_RST: u32 = 0x1a;
pub const TOD_FUCI_RST: u32 = 0x1b;
pub const SERDES_APB_LOGIC_RST: u32 = 0x1c;
pub const SERDES_APB_FUCI_RST: u32 = 0x1d;
pub const PON_CKG_CFG: u32 = 0x25;
pub const ETH_CKG_CFG: u32 = 0x26;
pub const PON_SERDES_RST: u32 = 0x27;
pub const ETH_SERDES_RST: u32 = 0x28;
pub const HSST_RST: u32 = 0x29;

pub const SHM_CFG_DEBUG_BASIC: u32 = 0xa07;
pub const SHM_CFG_MATRIX_INIT: u32 = 0xa0c;
pub const PCS_INIT: u32 = 0xd21;

pub const PD_PLL_SDS_CFG: u32 = 0x1104;
pub const PON_PLL_CFG: u32 = 0x1105;
pub const PON_PLL_FRAC_CFG: u32 = 0x1106;
pub const ETH_PLL_CFG: u32 = 0x1108;
pub const HSST_CFG: u32 = 0x110b;
pub const SERDES_STATUS: u32 = 0x110f;

pub const SERDES_LOCK_MASK: u32 = 0x0003_0001;
const SPI_MISO_DEFAULT: u32 = (47 << 8) | 7;
const PON_PLL_FRAC_DEFAULT: u32 = 0x006a_7ef9;
const PON_PLL_DEFAULT: u32 = 0x0006_507c;
const ETH_PLL_DEFAULT: u32 = 0x0006_507d;

const ETH_SERDES_BUS: u8 = 0;
const PON_SERDES_BUS: u8 = 1;
const EFUSE_APB_BUS: u8 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockSourceMode {
    /// Stock `g_chip_clock_souce_mode == 0`: writes `HSST_CFG = 1`, PON index 261 = `0xe9`, ETH index 261 = `0xed`.
    #[default]
    Default,
    /// Stock `g_chip_clock_souce_mode != 0`: skips `HSST_CFG`, PON/ETH index 261 = `0xc9`.
    Alternate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EthSerdesRate {
    /// Stock `g_serdes_rate_default_mode` (`[0x03, 0x00, 0x01, 0x00]`) 3.125G Ethernet SerDes.
    #[default]
    Rate3p125G,
    /// 1.25G Ethernet SerDes variant applying `ETH_SERDES_1P25G_PATCHES`.
    Rate1p25G,
}

#[derive(Clone, Copy, Debug)]
pub struct InitConfig {
    pub clock_source: ClockSourceMode,
    pub eth_rate: EthSerdesRate,
    pub max_onu_id: u16,
    pub gdba_scene: GdbaScene,
    pub serdes_lock_poll: PollPolicy,
    pub ram_init_poll: PollPolicy,
}

impl Default for InitConfig {
    fn default() -> Self {
        Self {
            clock_source: ClockSourceMode::Default,
            eth_rate: EthSerdesRate::Rate3p125G,
            max_onu_id: 128,
            gdba_scene: GdbaScene::TwoPerOnu,
            serdes_lock_poll: PollPolicy {
                max_reads: NonZeroU16::new(1000).unwrap(),
                interval: Duration::from_millis(1),
            },
            ram_init_poll: PollPolicy {
                max_reads: NonZeroU16::new(200).unwrap(),
                interval: Duration::from_millis(1),
            },
        }
    }
}

pub(super) fn set_field(words: &mut [u32], lsb: u32, width: u32, value: u32) {
    debug_assert!(width > 0 && width <= 32);
    let word_idx = (lsb / 32) as usize;
    let shift = lsb % 32;
    let mask = if width == 32 {
        u32::MAX
    } else {
        (1u32 << width) - 1
    };
    let v = value & mask;
    words[word_idx] = (words[word_idx] & !(mask << shift)) | (v << shift);
    if shift + width > 32 {
        let hi_bits = shift + width - 32;
        let hi_mask = (1u32 << hi_bits) - 1;
        let lo_bits = 32 - shift;
        words[word_idx + 1] = (words[word_idx + 1] & !hi_mask) | (v >> lo_bits);
    }
}

pub(super) fn write_reg_words<B: Bus>(
    fsl: &mut Fsl<B>,
    addr: u32,
    words: &[u32],
) -> io::Result<()> {
    fsl.write(RegisterSpace::Register, addr, 0, words)
}

pub(super) fn write_reg_u32<B: Bus>(fsl: &mut Fsl<B>, addr: u32, value: u32) -> io::Result<()> {
    write_reg_words(fsl, addr, &[value])
}

pub(super) fn rmw_fields<B: Bus>(
    fsl: &mut Fsl<B>,
    addr: u32,
    word_count: u8,
    fields: &[(u32, u32, u32)],
) -> io::Result<()> {
    let mut words = fsl.read(RegisterSpace::Register, addr, 0, usize::from(word_count))?;
    for &(lsb, width, value) in fields {
        set_field(&mut words, lsb, width, value);
    }
    write_reg_words(fsl, addr, &words)
}

/// Mirrors the stock GPON RAM test, but propagates errors and timeout. Bit6
/// (switch RAM) and bits8/9 (XGPON RAM) are deliberately not readiness criteria
/// in this stock test. A successful result is not optical/SerDes/OLT readiness.
pub fn wait_ram_initialized<B: Bus>(
    fsl: &mut Fsl<B>,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<u32> {
    polling::register(
        fsl,
        RAM_INIT_STATUS,
        GPON_RAM_INIT_MASK,
        GPON_RAM_INIT_MASK,
        policy,
        checkpoint,
    )
}

/// Pre-reset SPI timing and FUCI/logic release (`fsldrv_gpon_release_reset_pre`).
pub fn release_reset_pre<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<()> {
    write_reg_u32(fsl, GLOBAL_RST_FUCI, u32::MAX)?;
    write_reg_u32(fsl, SPI_MISO_CFG, SPI_MISO_DEFAULT)?;
    write_reg_u32(fsl, GLOBAL_RST_LOGIC, 1)
}

/// Programs PON 2.5G and ETH 3.125G/1.25G SerDes APB profiles plus the e-fuse
/// `0x30` affine transform and sets `ETH_TYPE_CFG = 0` (`fsldrv_gpon_sds_rate_mode_config` default path).
pub fn configure_serdes_rate_mode<B: Bus>(
    fsl: &mut Fsl<B>,
    clock_source: ClockSourceMode,
    eth_rate: EthSerdesRate,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    for (idx, &(addr, default_val)) in PON_SERDES_2P5G_WRITES.iter().enumerate() {
        let val = if idx == PON_SERDES_CLK_PATCH_INDEX && clock_source == ClockSourceMode::Alternate
        {
            PON_SERDES_CLK_MODE1_VALUE
        } else {
            default_val
        };
        serdes_apb::write_on(fsl, PON_SERDES_BUS, addr, val, checkpoint)?;
    }

    for (idx, &(addr, default_val)) in ETH_SERDES_3P125G_WRITES.iter().enumerate() {
        let mut val = default_val;
        if idx == ETH_SERDES_CLK_PATCH_INDEX && clock_source == ClockSourceMode::Alternate {
            val = ETH_SERDES_CLK_MODE1_VALUE;
        } else if eth_rate == EthSerdesRate::Rate1p25G {
            if let Some(&(_, patch_addr, patch_val)) = ETH_SERDES_1P25G_PATCHES
                .iter()
                .find(|&&(patch_idx, _, _)| patch_idx == idx)
            {
                debug_assert_eq!(patch_addr, addr);
                val = patch_val;
            }
        }
        serdes_apb::write_on(fsl, ETH_SERDES_BUS, addr, val, checkpoint)?;
    }

    let raw_efuse = serdes_apb::read_on(fsl, EFUSE_APB_BUS, ETH_EFUSE_ADDRESS, checkpoint)?;
    serdes_apb::write_on(
        fsl,
        EFUSE_APB_BUS,
        ETH_EFUSE_ADDRESS,
        efuse_0x30_transform(raw_efuse),
        checkpoint,
    )?;

    rmw_fields(fsl, ETH_TYPE_CFG, 1, &[(0, 2, 0)])
}

/// Enables MAU debug and seeds the shared memory allocation matrix from
/// `max_onu_id * gdba_scene.stock_code()` (`fsldrv_gpon_mau_config`).
pub fn configure_mau<B: Bus>(
    fsl: &mut Fsl<B>,
    max_onu_id: u16,
    gdba_scene: GdbaScene,
) -> io::Result<()> {
    rmw_fields(fsl, SHM_CFG_DEBUG_BASIC, 2, &[(42, 1, 1)])?;
    let total_tconts = u32::from(max_onu_id) * u32::from(gdba_scene.stock_code());
    let matrix = if total_tconts <= 64 {
        [0x0f00_ffff, 0x00f0]
    } else if total_tconts <= 128 {
        [0xff0f_f0ff, 0x0ff0]
    } else {
        [u32::MAX, 0xffff]
    };
    write_reg_words(fsl, SHM_CFG_MATRIX_INIT, &matrix)
}

/// Releases Ethernet SerDes PCS/XSBI/XGXS logic resets based on `ETH_TYPE_CFG`
/// (`fsldrv_gpon_common_release_eth_sds_logic`).
pub fn release_eth_sds_logic<B: Bus>(fsl: &mut Fsl<B>, eth_type_cfg: u8) -> io::Result<()> {
    rmw_fields(fsl, ETH_SERDES_RX_DIV_LOGIC_RST, 1, &[(0, 1, 1)])?;
    let masked = u32::from(eth_type_cfg & 0xfd);
    if masked == 1 {
        rmw_fields(
            fsl,
            ETH_SERDES_RX_LOGIC_RST,
            1,
            &[(1, 1, masked), (0, 1, 0)],
        )?;
        rmw_fields(
            fsl,
            ETH_SERDES_TX_LOGIC_RST,
            1,
            &[(1, 1, masked), (0, 1, 0)],
        )
    } else {
        rmw_fields(fsl, ETH_SERDES_RX_LOGIC_RST, 1, &[(1, 1, 0), (0, 1, 1)])?;
        rmw_fields(fsl, ETH_SERDES_TX_LOGIC_RST, 1, &[(1, 1, 0), (0, 1, 1)])
    }
}

fn pulse_eth_serdes_logic_resets<B: Bus>(fsl: &mut Fsl<B>, release: u32) -> io::Result<()> {
    rmw_fields(
        fsl,
        ETH_SYS_LOGIC_RST,
        1,
        &[(2, 1, release), (1, 1, release)],
    )?;
    rmw_fields(
        fsl,
        ETH_SERDES_TX_LOGIC_RST,
        1,
        &[(0, 1, release), (1, 1, release)],
    )?;
    rmw_fields(fsl, ETH_SERDES_RX_DIV_LOGIC_RST, 1, &[(0, 1, release)])?;
    rmw_fields(
        fsl,
        ETH_SERDES_RX_LOGIC_RST,
        1,
        &[(0, 1, release), (1, 1, release)],
    )
}

/// Polls `SERDES_STATUS` (`0x110f`) for both PON/ETH SerDes readiness under a
/// bounded `PollPolicy`, toggles the SerDes logic resets, and releases `PCS_INIT`
/// (`fsldrv_gpon_common_serdes_logic_reset_and_release`).
pub fn reset_and_release_serdes_logic<B: Bus>(
    fsl: &mut Fsl<B>,
    clock_source: ClockSourceMode,
    lock_poll: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    checkpoint(Duration::ZERO)?;
    if clock_source == ClockSourceMode::Default {
        write_reg_u32(fsl, HSST_CFG, 1)?;
    }
    polling::register(
        fsl,
        SERDES_STATUS,
        SERDES_LOCK_MASK,
        SERDES_LOCK_MASK,
        lock_poll,
        checkpoint,
    )?;

    write_reg_u32(fsl, PON_SERDES_US_LOGIC_RST, 0)?;
    write_reg_u32(fsl, PON_SERDES_DS_LOGIC_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, PON_SERDES_US_LOGIC_RST, u32::MAX)?;
    write_reg_u32(fsl, PON_SERDES_DS_LOGIC_RST, u32::MAX)?;

    pulse_eth_serdes_logic_resets(fsl, 0)?;
    checkpoint(Duration::from_millis(1))?;
    pulse_eth_serdes_logic_resets(fsl, 1)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, PCS_INIT, 0xff)
}

/// Executes the full FSL reset-release, PLL, SerDes IP, MAU, and RAM-init
/// sequence (`fsldrv_gpon_release_reset`).
pub fn release_reset<B: Bus>(
    fsl: &mut Fsl<B>,
    config: &InitConfig,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    checkpoint(Duration::ZERO)?;
    write_reg_u32(fsl, GLOBAL_RST_FUCI, 0)?;
    write_reg_u32(fsl, TOD_FUCI_RST, 0)?;
    write_reg_u32(fsl, SERDES_APB_FUCI_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;

    write_reg_u32(fsl, GLOBAL_RST_FUCI, u32::MAX)?;
    write_reg_u32(fsl, TOD_FUCI_RST, u32::MAX)?;
    write_reg_u32(fsl, SERDES_APB_FUCI_RST, u32::MAX)?;

    write_reg_u32(fsl, PON_CKG_CFG, 10)?;
    write_reg_u32(fsl, ETH_CKG_CFG, 15)?;
    write_reg_u32(fsl, PON_SYS_FUCI_RST, u32::MAX)?;
    write_reg_u32(fsl, PON_TYPE_CFG, 0)?;
    write_reg_u32(fsl, ETH_TYPE_CFG, 0)?;

    write_reg_u32(fsl, GLOBAL_RST_LOGIC, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, GLOBAL_RST_LOGIC, u32::MAX)?;
    write_reg_u32(fsl, SPI_MISO_CFG, SPI_MISO_DEFAULT)?;

    write_reg_u32(fsl, HSST_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    rmw_fields(fsl, HSST_RST, 1, &[(0, 1, 1)])?;
    checkpoint(Duration::from_millis(1))?;
    rmw_fields(fsl, HSST_RST, 1, &[(1, 1, 1)])?;

    write_reg_u32(fsl, SERDES_APB_LOGIC_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, SERDES_APB_LOGIC_RST, u32::MAX)?;

    configure_serdes_rate_mode(fsl, config.clock_source, config.eth_rate, checkpoint)?;
    checkpoint(Duration::from_millis(1))?;

    write_reg_u32(fsl, PON_PLL_FRAC_CFG, PON_PLL_FRAC_DEFAULT)?;
    write_reg_u32(fsl, PON_PLL_CFG, PON_PLL_DEFAULT)?;
    rmw_fields(fsl, PD_PLL_SDS_CFG, 1, &[(0, 9, 128)])?;
    rmw_fields(fsl, PD_PLL_SDS_CFG, 1, &[(0, 9, 0)])?;
    checkpoint(Duration::from_millis(1))?;

    write_reg_u32(fsl, ETH_PLL_CFG, ETH_PLL_DEFAULT)?;
    rmw_fields(fsl, PD_PLL_SDS_CFG, 1, &[(0, 9, 8)])?;
    rmw_fields(fsl, PD_PLL_SDS_CFG, 1, &[(0, 9, 0)])?;
    checkpoint(Duration::from_millis(1))?;

    write_reg_u32(fsl, PON_SERDES_RST, 15)?;
    write_reg_u32(fsl, ETH_SERDES_RST, 15)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, PON_SERDES_RST, 0)?;
    write_reg_u32(fsl, ETH_SERDES_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;

    configure_mau(fsl, config.max_onu_id, config.gdba_scene)?;
    let eth_type = fsl.read_register(ETH_TYPE_CFG)? as u8;
    release_eth_sds_logic(fsl, eth_type)?;

    write_reg_u32(fsl, ETH_SYS_FUCI_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, ETH_SYS_FUCI_RST, u32::MAX)?;

    write_reg_u32(fsl, ETH_SYS_LOGIC_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, ETH_SYS_LOGIC_RST, u32::MAX)?;

    rmw_fields(fsl, PON_SYS_LOGIC_RST, 1, &[(3, 1, 1), (4, 1, 1)])?;
    rmw_fields(fsl, PON_SYS_LOGIC_RST, 1, &[(0, 1, 1)])?;

    write_reg_u32(fsl, PON_SERDES_US_LOGIC_RST, 0)?;
    write_reg_u32(fsl, PON_SERDES_DS_LOGIC_RST, 0)?;
    checkpoint(Duration::from_millis(1))?;
    write_reg_u32(fsl, PON_SERDES_US_LOGIC_RST, u32::MAX)?;
    write_reg_u32(fsl, PON_SERDES_DS_LOGIC_RST, u32::MAX)?;

    write_reg_u32(fsl, TOD_LOGIC_RST, u32::MAX)?;
    checkpoint(Duration::from_millis(1))?;

    rmw_fields(
        fsl,
        PON_SYS_LOGIC_RST,
        1,
        &[
            (23, 1, 1),
            (1, 1, 1),
            (5, 1, 1),
            (14, 1, 1),
            (15, 1, 1),
            (16, 1, 1),
            (17, 1, 1),
            (6, 1, 1),
            (7, 1, 1),
            (8, 1, 1),
            (9, 1, 1),
            (10, 1, 1),
            (11, 1, 1),
        ],
    )?;
    checkpoint(Duration::from_millis(10))?;

    reset_and_release_serdes_logic(
        fsl,
        config.clock_source,
        config.serdes_lock_poll,
        checkpoint,
    )?;

    let eth_ckg = if (u32::from(eth_type) & 0xfd) == 1 {
        12
    } else {
        3
    };
    write_reg_u32(fsl, ETH_CKG_CFG, eth_ckg)?;
    wait_ram_initialized(fsl, config.ram_init_poll, checkpoint)?;
    checkpoint(Duration::from_millis(200))
}

#[cfg(test)]
mod tests;
