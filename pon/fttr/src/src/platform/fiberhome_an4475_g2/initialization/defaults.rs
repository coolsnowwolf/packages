//! Static GPON MAC, GDBA, BMP, NNI, ToD, and default GEM configuration
//! sequences extracted from `fslstack_gpon_reg_config`.

use super::{rmw_fields, set_field, write_reg_u32, write_reg_words, ETH_TYPE_CFG, PD_PLL_SDS_CFG};
use crate::platform::{
    diagnostics::RegisterSpace,
    fiberhome_an4475_g2::{
        activation::{
            configure_gem_port, set_gem_wmci_aes, ACTIVE_REG_WORDS, BMP_CFG_ONU_ACTIVE,
            MAX_ONU_INDEX,
        },
        gdba::{GDBA_B_ALL_CFG, GDBA_WORK_CFG},
        protocol::{Bus, Fsl, OPT_CONFIG},
    },
};
use std::io;

pub const DEBUG_PIN_SEL: u32 = 0x3;
pub const PON_SDS_CONFIG_EN_CFG: u32 = 0x100;
pub const PON_SDS_CONFIG_CFG: u32 = 0x101;
pub const PON_SDS_AFIFO_DELAY_CFG: u32 = 0x108;
pub const PON_SDS_AFIFO_DELAY_XG_CFG: u32 = 0x109;
pub const PON_SDS_OVERSAMP_SEL_DLY_CFG: u32 = 0x11c;

pub const GPON_OVERHEAD_CFG: u32 = 0x203;
pub const GPON_EXTEND_BURST_CFG: u32 = 0x206;
pub const GPON_GUARD_TIME_CFG: u32 = 0x207;
pub const GPON_OPT_SD_CFG: u32 = 0x208;
pub const GPON_OPT_RESET_CFG: u32 = 0x209;
pub const GPON_SERDES_CFG: u32 = 0x20a;
pub const GPON_DELIMITER_MATCH_BIT_CFG: u32 = 0x20b;
pub const GPON_DRIFT_TH_CFG: u32 = 0x20e;
pub const GPON_ONUID_OFFSET_CFG: u32 = 0x21a;
pub const GPON_OVERHEAD_2P5_CFG: u32 = 0x21b;
pub const GPON_EXTEND_BURST_2P5_CFG: u32 = 0x21e;
pub const GPON_LIGHT_ON_CFG: u32 = 0x223;
pub const GPON_US_FEC_EN_CFG: u32 = 0x22d;

pub const PLOAM_PROFILE_MODLE_CFG: u32 = 0x410;

pub const GDBA_DBA_START_CFG: u32 = 0x501;
pub const GDBA_SLOT_REQ_CFG: u32 = 0x502;
pub const GDBA_SLOT_TAB: u32 = 0x504;
pub const GDBA_CW_CFG: u32 = 0x50b;
pub const GDBA_BASIC_CFG: u32 = 0x510;
pub const GDBA_L_H_THR_CFG: u32 = 0x512;
pub const GDBA_DS_CNT_CFG: u32 = 0x519;
pub const GDBA_ONU_STORAGE_LINE1_CFG: u32 = 0x525;
pub const GDBA_ONU_STORAGE_LINE8_CFG: u32 = 0x52c;
pub const GDBA_SLC_PARA_TAB: u32 = 0x53f;

pub const BMP_CFG_SN_REQ: u32 = 0x606;
pub const BMP_OMCI_ALLOC_VALUE: u32 = 0x60a;
pub const BMP_CFG_BOUNDS: u32 = 0x617;
pub const BMP_CFG_PLOAMU_CTRL: u32 = 0x637;

pub const VLAN_TYPE_CFG: u32 = 0x6;

pub const NNI_OMCI_DA_CFG: u32 = 0x900;
pub const NNI_OMCI_TYPE_CFG: u32 = 0x905;
pub const NNI_ETH_TPID_CFG: u32 = 0x906;
pub const NNI_ETH_SIZE_CFG: u32 = 0x907;
pub const NNI_ERR_DROP_CFG: u32 = 0x90a;
pub const NNI_TX_PAUSE_THD_CFG: u32 = 0x90b;
pub const NNI_OMCI_WMCI_SPI_CFG: u32 = 0x913;
pub const NNI_INT_MASK: u32 = 0x915;
pub const NNI_WMCI_TYPE_CFG: u32 = 0x91e;
pub const NNI_GEMPORT_OFFSET_CFG: u32 = 0x9e6;

pub const DEFAULT_NNI_ETH_TPID: u16 = 0x8200;
pub const STANDARD_8021Q_TPID: u16 = 0x8100;
pub const IEEE_8021AD_TPID: u16 = 0x88a8;
pub const DEFAULT_NNI_GEMPORT_OFFSET: u16 = 255;

pub const TOD_UART_BAUD_RATE_CFG: u32 = 0x1203;
pub const TOD_NS_CARRY_LENGTH_CFG: u32 = 0x1204;
pub const TOD_TIME_UNIT_CFG: u32 = 0x1207;
pub const TOD_DEL_BURR_CFG: u32 = 0x120d;

const MAX_SUPPORTED_ONUS: u16 = MAX_ONU_INDEX + 1;
const GDBA_ONU_STORAGE_LINES: [u32; 8] = [2430, 4860, 7290, 9720, 12150, 14580, 19440, 77760];

fn validate_max_onu_id(max_onu_id: u16) -> io::Result<()> {
    if max_onu_id == 0 || max_onu_id > MAX_SUPPORTED_ONUS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_onu_id must be within 1..=128",
        ));
    }
    Ok(())
}

/// Applies static GPON MAC/PHY/NNI/ToD defaults (`fsldrv_gpon_reg_config`),
/// returning the hardware `ONUID_OFFSET_CFG` byte.
pub fn apply_reg_config<B: Bus>(fsl: &mut Fsl<B>, max_onu_id: u16) -> io::Result<u8> {
    validate_max_onu_id(max_onu_id)?;

    rmw_fields(fsl, GPON_OVERHEAD_CFG, 3, &[(0, 8, 32)])?;
    rmw_fields(fsl, GPON_OVERHEAD_2P5_CFG, 3, &[(0, 8, 32)])?;
    rmw_fields(
        fsl,
        OPT_CONFIG,
        1,
        &[(4, 1, 1), (7, 5, 5), (0, 1, 1), (5, 1, 0)],
    )?;
    rmw_fields(
        fsl,
        PON_SDS_CONFIG_CFG,
        1,
        &[
            (12, 1, 1),
            (13, 1, 1),
            (0, 8, 0),
            (8, 3, 7),
            (11, 1, 1),
            (15, 3, 2),
            (14, 1, 0),
            (29, 1, 1),
        ],
    )?;
    rmw_fields(fsl, PON_SDS_CONFIG_EN_CFG, 1, &[(0, 1, 1)])?;
    rmw_fields(fsl, GPON_DELIMITER_MATCH_BIT_CFG, 1, &[(0, 5, 24)])?;
    rmw_fields(
        fsl,
        GPON_SERDES_CFG,
        1,
        &[(0, 7, 53), (7, 7, 37), (14, 7, 43)],
    )?;
    rmw_fields(fsl, GPON_GUARD_TIME_CFG, 1, &[(0, 8, 16), (8, 8, 32)])?;
    rmw_fields(fsl, GPON_EXTEND_BURST_CFG, 1, &[(0, 8, 80), (8, 8, 80)])?;
    rmw_fields(fsl, GPON_EXTEND_BURST_2P5_CFG, 1, &[(0, 8, 80), (8, 8, 80)])?;
    rmw_fields(
        fsl,
        PLOAM_PROFILE_MODLE_CFG,
        1,
        &[(0, 14, 4095), (22, 1, 1)],
    )?;
    rmw_fields(fsl, GPON_OPT_RESET_CFG, 1, &[(7, 7, 4), (14, 5, 10)])?;
    rmw_fields(fsl, GPON_OPT_SD_CFG, 1, &[(9, 8, 160)])?;
    rmw_fields(fsl, PON_SDS_AFIFO_DELAY_CFG, 1, &[(10, 10, 16)])?;
    rmw_fields(fsl, PON_SDS_AFIFO_DELAY_XG_CFG, 1, &[(10, 10, 10)])?;
    rmw_fields(fsl, BMP_CFG_SN_REQ, 2, &[(0, 11, 16)])?;
    rmw_fields(
        fsl,
        BMP_OMCI_ALLOC_VALUE,
        1,
        &[(0, 7, u32::from(max_onu_id - 1))],
    )?;
    write_reg_words(fsl, BMP_CFG_ONU_ACTIVE, &[0; ACTIVE_REG_WORDS])?;
    write_reg_u32(fsl, NNI_INT_MASK, 0xff)?;
    rmw_fields(fsl, GPON_LIGHT_ON_CFG, 1, &[(21, 10, 16), (0, 16, 0x1fff)])?;
    rmw_fields(
        fsl,
        GPON_DRIFT_TH_CFG,
        1,
        &[(0, 8, 18), (8, 8, 36), (16, 5, 8)],
    )?;
    let onu_id_offset = fsl.read_register(GPON_ONUID_OFFSET_CFG)? as u8;
    rmw_fields(fsl, GPON_US_FEC_EN_CFG, 1, &[(0, 1, 1)])?;
    rmw_fields(fsl, PON_SDS_OVERSAMP_SEL_DLY_CFG, 1, &[(0, 6, 10)])?;
    rmw_fields(fsl, NNI_TX_PAUSE_THD_CFG, 1, &[(0, 10, 248)])?;
    rmw_fields(fsl, NNI_OMCI_TYPE_CFG, 1, &[(0, 1, 1)])?;
    rmw_fields(fsl, NNI_WMCI_TYPE_CFG, 1, &[(0, 1, 1)])?;
    write_reg_u32(fsl, NNI_ERR_DROP_CFG, 1)?;
    rmw_fields(fsl, NNI_OMCI_DA_CFG, 2, &[(48, 1, 1)])?;
    rmw_fields(fsl, PD_PLL_SDS_CFG, 1, &[(9, 16, 1536)])?;
    rmw_fields(fsl, DEBUG_PIN_SEL, 1, &[(0, 3, 2), (3, 2, 1)])?;
    rmw_fields(fsl, TOD_UART_BAUD_RATE_CFG, 1, &[(0, 20, 16200)])?;
    rmw_fields(fsl, TOD_NS_CARRY_LENGTH_CFG, 1, &[(0, 30, 155_519_999)])?;
    rmw_fields(fsl, TOD_TIME_UNIT_CFG, 1, &[(0, 32, 19440)])?;
    rmw_fields(fsl, TOD_DEL_BURR_CFG, 1, &[(16, 16, 2), (0, 16, 16)])?;
    Ok(onu_id_offset)
}

/// Applies static GDBA defaults (`fsldrv_gpon_gdba_default_config`).
pub fn apply_gdba_defaults<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<()> {
    rmw_fields(fsl, GDBA_BASIC_CFG, 1, &[(6, 6, 7)])?;
    rmw_fields(fsl, GDBA_DS_CNT_CFG, 1, &[(0, 1, 1), (7, 15, 20)])?;
    rmw_fields(fsl, GDBA_L_H_THR_CFG, 4, &[(0, 15, 20)])?;
    rmw_fields(fsl, GDBA_CW_CFG, 2, &[(32, 16, 6), (2, 16, 6), (0, 1, 1)])?;
    rmw_fields(fsl, GDBA_DBA_START_CFG, 1, &[(0, 1, 1)])?;
    rmw_fields(fsl, GDBA_WORK_CFG, 1, &[(1, 3, 0), (0, 1, 1)])?;
    rmw_fields(
        fsl,
        GDBA_B_ALL_CFG,
        2,
        &[(57, 1, 1), (0, 16, 38880), (32, 16, 38880)],
    )?;

    let mut slot_word = [0u32; 1];
    set_field(&mut slot_word, 6, 2, 0);
    set_field(&mut slot_word, 1, 5, 31);
    set_field(&mut slot_word, 0, 1, 1);
    fsl.write(RegisterSpace::Memory, GDBA_SLOT_TAB, 0, &slot_word)?;
    write_reg_u32(fsl, GDBA_SLOT_REQ_CFG, 1)?;

    for row in 0..4u32 {
        fsl.write(RegisterSpace::Memory, GDBA_SLC_PARA_TAB, row, &[u32::MAX])?;
    }
    for (idx, &val) in GDBA_ONU_STORAGE_LINES.iter().enumerate() {
        write_reg_u32(fsl, GDBA_ONU_STORAGE_LINE1_CFG + idx as u32, val)?;
    }
    Ok(())
}

/// Applies default BMP start bound and clears forced PLOAMu bitmap
/// (`fsldrv_gpon_bmp_default_config`).
pub fn apply_bmp_defaults<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<()> {
    rmw_fields(fsl, BMP_CFG_BOUNDS, 2, &[(14, 14, 9719)])?;
    write_reg_words(fsl, BMP_CFG_PLOAMU_CTRL, &[0; 8])
}

/// Applies default NNI Ethernet size and WMCI CRC modes (`fsldrv_gpon_nni_default_config`).
pub fn apply_nni_defaults<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<()> {
    rmw_fields(fsl, NNI_ETH_SIZE_CFG, 1, &[(14, 7, 30)])?;
    rmw_fields(fsl, NNI_OMCI_WMCI_SPI_CFG, 1, &[(5, 2, 2), (3, 2, 2)])
}

/// Populates default per-ONU GEM ports (`256 + 16*onu + 0..16`, with WMCI enabled
/// on slot 14) and each ONU's OMCI GEM port (`gem == onu_idx`) (`fsldrv_gpon_gemport_default_config`).
pub fn apply_gemport_defaults<B: Bus>(fsl: &mut Fsl<B>, max_onu_id: u16) -> io::Result<()> {
    validate_max_onu_id(max_onu_id)?;
    for onu_idx in 0..max_onu_id {
        let base = 256 + 16 * onu_idx;
        for slot in 0..16 {
            let gem = base + slot;
            configure_gem_port(fsl, onu_idx, 0, gem, true)?;
            if slot == 14 {
                set_gem_wmci_aes(fsl, gem, true, false)?;
            }
        }
        configure_gem_port(fsl, onu_idx, 0, onu_idx, true)?;
    }
    Ok(())
}

/// Configures the NNI Ethernet TPID (`fsldrv_gpon_common_setnniethtpid`).
/// Stock `fh_olt_pon_vport_init` programs `0x8200`; standard Linux 802.1Q uses `0x8100`.
pub fn set_nni_eth_tpid<B: Bus>(fsl: &mut Fsl<B>, tpid: u16) -> io::Result<()> {
    rmw_fields(fsl, NNI_ETH_TPID_CFG, 1, &[(0, 16, u32::from(tpid))])?;
    write_reg_u32(fsl, VLAN_TYPE_CFG, u32::from(tpid))
}

/// Configures the NNI GEM-port-to-VID offset (`fsldrv_gpon_common_setnnigemportoffset`).
/// Stock `fh_olt_pon_vport_init` programs `255` (`0x0ff`), 12-bit max `0xfff`.
pub fn set_nni_gem_port_offset<B: Bus>(fsl: &mut Fsl<B>, offset: u16) -> io::Result<()> {
    if offset > 0x0fff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NNI GEM port offset must fit in 12 bits (<= 0xfff)",
        ));
    }
    rmw_fields(
        fsl,
        NNI_GEMPORT_OFFSET_CFG,
        1,
        &[(0, 12, u32::from(offset))],
    )
}

/// Applies NNI virtual-port encapsulation parameters (`fh_olt_pon_vport_init`).
pub fn configure_nni_vport<B: Bus>(
    fsl: &mut Fsl<B>,
    gem_port_offset: u16,
    eth_tpid: u16,
) -> io::Result<()> {
    set_nni_gem_port_offset(fsl, gem_port_offset)?;
    set_nni_eth_tpid(fsl, eth_tpid)
}

/// Configures the FSL debug/GPIO pins used for board-level optical/RSSI control
/// (`fsldrv_gpon_rssiadctrl` -> `fsldrv_gpon_common_gpio_set`).
///
/// Stock `subont_manager` invokes `fhdrv_olt_pon_rssi_ad_ctrl(0xff, 0xff, 1)`
/// before enabling optical TX, which masks `ETH_TYPE_CFG@0x2` to bit 0 and
/// updates `DEBUG_PIN_SEL@0x3` with `GPIO_TS=0`, `GPIO_SEL=4`, and
/// `GPIO_LED[3:2]=1` (`0x8c` from the default `0x0a`).
pub fn configure_rssi_ad_ctrl<B: Bus>(
    fsl: &mut Fsl<B>,
    rssi_thxor: Option<bool>,
    rssi_gain: Option<bool>,
    rssi_ad_en: Option<bool>,
) -> io::Result<()> {
    let eth_type = fsl.read_register(ETH_TYPE_CFG)?;
    write_reg_u32(fsl, ETH_TYPE_CFG, eth_type & 1)?;

    let mut debug_pin = [fsl.read_register(DEBUG_PIN_SEL)?];
    set_field(&mut debug_pin, 15, 5, 0); // GPIO_TS = 0
    set_field(&mut debug_pin, 0, 3, 4); // GPIO_SEL = 4
    let mut gpio_led = (debug_pin[0] >> 5) & 0x3ff;
    if let Some(thxor) = rssi_thxor {
        gpio_led = (gpio_led & 0x33f) | (u32::from(thxor) << 6);
    }
    if let Some(gain) = rssi_gain {
        gpio_led = (gpio_led & 0x3cf) | (u32::from(gain) << 4);
    }
    if let Some(ad_en) = rssi_ad_en {
        gpio_led = (gpio_led & 0x3f3) | (u32::from(ad_en) << 2);
    }
    set_field(&mut debug_pin, 5, 10, gpio_led);
    write_reg_u32(fsl, DEBUG_PIN_SEL, debug_pin[0])
}

/// Runs the full `fslstack_gpon_reg_config` sequence and returns `ONUID_OFFSET_CFG`.
pub fn apply_gpon_defaults<B: Bus>(fsl: &mut Fsl<B>, max_onu_id: u16) -> io::Result<u8> {
    let onu_id_offset = apply_reg_config(fsl, max_onu_id)?;
    apply_gdba_defaults(fsl)?;
    apply_bmp_defaults(fsl)?;
    apply_nni_defaults(fsl)?;
    apply_gemport_defaults(fsl, max_onu_id)?;
    Ok(onu_id_offset)
}
