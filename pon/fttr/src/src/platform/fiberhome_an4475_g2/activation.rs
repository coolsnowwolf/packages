//! Stock GPON ONU ranging, OMCC activation, and GEM forwarding table primitives.
//!
//! Recovered from `fsl_core_fttr_chip.ko` (`fslstack_gpon_add_onu_o4_o5_process`,
//! `fslstack_gpon_ranging_time`, `fslstack_gpon_configure_port_id`,
//! `fsldrv_gpon_bmp_onu_active_conf`, `fsldrv_gpon_alm_pro_onu_active_config`,
//! `fsldrv_gpon_onuid_to_onuindex`, `fsldrv_gpon_gemport_info_conf`,
//! `fsldrv_gpon_range_request`, `fsldrv_gpon_common_us_wmci_cfg`, and
//! `fsldrv_gpon_common_switch_gemport_bitmap`).
//! These are low-level register/table primitives only; complete OLT activation
//! orchestration still requires DBA/T-CONT scene closure and hardware testing.

use super::{
    downstream::SendError,
    management::Management,
    polling::{self, PollPolicy},
    protocol::{Bus, Fsl, MAX_ROW},
};
use crate::platform::diagnostics::RegisterSpace;
use std::{io, time::Duration};

/// `BMP_CFG_ONU_ACTIVE` (reg ID `0x1`, 128-bit / 4 words, base address `0x602`).
pub const BMP_CFG_ONU_ACTIVE: u32 = 0x602;
/// `BMP_CFG_RG_REQ` (reg ID `0x6`, 42-bit / 2 words, base address `0x60f`).
pub const BMP_CFG_RG_REQ: u32 = 0x60f;
/// `BWMAP_CFG_BASIC` (reg ID `0x9`, 57-bit / 2 words, base address `0x619`).
pub const BWMAP_CFG_BASIC: u32 = 0x619;
/// `BMP_PLS_REQ` (reg ID `0x11`, 4-bit / 1 word, base address `0x61d`).
pub const BMP_PLS_REQ: u32 = 0x61d;
/// `BMP_STA` (reg ID `0x18`, 5-bit / 1 word, base address `0x624`).
pub const BMP_STA: u32 = 0x624;
/// `BMP_CFG_UP_RATE_IND` (reg ID `0xe`, 128-bit / 4 words, base address `0x62f`).
pub const BMP_CFG_UP_RATE_IND: u32 = 0x62f;
/// `BMP_CFG_ONU_FEC_1G` (reg ID `0xf`, 128-bit / 4 words, base address `0x633`).
pub const BMP_CFG_ONU_FEC_1G: u32 = 0x633;
/// `BMP_CFG_PLOAMU_CTRL` (reg ID `0x10`, 256-bit / 8 words, base address `0x637`).
pub const BMP_CFG_PLOAMU_CTRL: u32 = 0x637;
/// `GPON_US_RATE_SEL_CFG` (reg ID `0xba`, 128-bit / 4 words, base address `0x212`).
pub const GPON_US_RATE_SEL_CFG: u32 = 0x212;
/// `GPON_ALM_PROC_ONU_ACTIVE_CFG` (reg ID `0xc6`, 128-bit / 4 words, base address `0x228`).
pub const ALM_PROC_ONU_ACTIVE_CFG: u32 = 0x228;
/// `PLOAM_US_FILTER_CFG` (reg ID `0x147`, 4 x 17-bit entries, base address `0x458..=0x45b`).
pub const PLOAM_US_FILTER_CFG: u32 = 0x458;
/// `RSM_CFG_TCONT_CLR` (reg ID `0x159`, 9-bit / 1 word, base address `0x800`).
pub const RSM_CFG_TCONT_CLR: u32 = 0x800;
/// `RSM_ALM` (reg ID `0x165`, 23-bit / 1 word, base address `0x810`).
pub const RSM_ALM: u32 = 0x810;
/// `PORTID_CFG_TAB` (mem ID `0xf`, 1024 x 32-bit rows, base address `0x48a`).
pub const PORTID_CFG_TAB: u32 = 0x48a;
/// `GPON_UP_WMCI_AES_EN_CFG_TAB` (mem ID `0x14`, 512 x 16-bit rows, base address `0x4a7`).
pub const UP_WMCI_AES_EN_CFG_TAB: u32 = 0x4a7;
/// `STATIS_US_GEM_EN_TAB` (mem ID `0x17`, 4096 x 9-bit rows, base address `0x81a`).
pub const STATIS_US_GEM_EN_TAB: u32 = 0x81a;
/// `STATIS_DS_GEM_EN_TAB` (mem ID `0x1f`, 4096 x 9-bit rows, base address `0x767`).
pub const STATIS_DS_GEM_EN_TAB: u32 = 0x767;
/// `SWITCH_CFG_GEMPORT_BITMAP` (reg ID `0x18c`, 64 x 64-bit entries, base address `0xb01`).
pub const SWITCH_CFG_GEMPORT_BITMAP: u32 = 0xb01;

pub const PLOAM_MSG_ASSIGN_ONU_ID: u8 = 0x03;
pub const PLOAM_MSG_RANGING_TIME: u8 = 0x04;
pub const PLOAM_MSG_DEACTIVATE_ONU_ID: u8 = 0x05;
pub const PLOAM_MSG_ACK: u8 = 0x09;
pub const PLOAM_MSG_ASSIGN_ALLOC_ID: u8 = 0x0a;
pub const PLOAM_MSG_CONFIGURE_PORT_ID: u8 = 0x0e;
pub const PLOAM_MSG_BER_INTERVAL: u8 = 0x12;
pub const DEFAULT_BER_INTERVAL: u32 = 28_800_000;
pub const PLOAM_DEFAULT_REPEAT: u8 = 3;
pub const MAX_PLOAM_FILTER_SLOTS: u8 = 4;
pub const MAX_ONU_INDEX: u16 = 127;
pub const ACTIVE_REG_WORDS: usize = 4;
const BMP_STA_RANGE_ACK: u32 = 1 << 3;
const BMP_PLS_RG_REQ: u32 = 1 << 0;
const RSM_ALM_TCONT_CLR_FAIL: u32 = 1 << 22;

/// Computes the EqD value used by `fslstack_gpon_add_onu_o4_o5_process`:
/// Type 7 (2.5G) uses `(raw >> 1) + 0x18`; type 1 (1.25G) uses raw.
pub fn eqd_from_ranging(raw_ranging_time: u32, is_2p5g: bool) -> u32 {
    if is_2p5g {
        (raw_ranging_time >> 1).saturating_add(0x18)
    } else {
        raw_ranging_time
    }
}

/// Builds a 12-byte downstream GPON `Assign_ONU-ID` (`0x03`) PLOAM frame
/// matching `fslstack_gpon_assign_onu_id`.
pub fn assign_onu_id_ploam(onu_id: u8, serial_number: &[u8; 8]) -> [u8; 12] {
    let mut frame = [0u8; 12];
    frame[0] = 0xff;
    frame[1] = PLOAM_MSG_ASSIGN_ONU_ID;
    frame[2] = onu_id;
    frame[3..11].copy_from_slice(serial_number);
    frame
}

/// Builds a 12-byte downstream GPON `Deactivate_ONU-ID` (`0x05`) PLOAM frame
/// matching `fslstack_gpon_deactive_onu`.
pub fn deactivate_onu_ploam(onu_id: u8) -> [u8; 12] {
    let mut frame = [0u8; 12];
    frame[0] = onu_id;
    frame[1] = PLOAM_MSG_DEACTIVATE_ONU_ID;
    frame
}

/// Builds a 12-byte downstream GPON `Ranging_Time` (`0x04`) PLOAM frame.
pub fn ranging_time_ploam(onu_id: u8, eqd: u32) -> [u8; 12] {
    let mut frame = [0u8; 12];
    frame[0] = onu_id;
    frame[1] = PLOAM_MSG_RANGING_TIME;
    frame[3..7].copy_from_slice(&eqd.to_be_bytes());
    frame
}

/// Builds a 12-byte downstream GPON `Configure_Port_ID` (`0x0e`) PLOAM frame.
pub fn configure_port_id_ploam(
    onu_id: u8,
    gem_port_id: u16,
    activate: bool,
    sequence: u16,
) -> io::Result<[u8; 12]> {
    if u32::from(gem_port_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GEM Port-ID exceeds 12-bit GPON range",
        ));
    }
    let mut frame = [0u8; 12];
    frame[0] = onu_id;
    frame[1] = PLOAM_MSG_CONFIGURE_PORT_ID;
    frame[2] = u8::from(activate);
    frame[3] = (gem_port_id >> 4) as u8;
    frame[4] = ((gem_port_id & 0xf) << 4) as u8;
    frame[5..7].copy_from_slice(&sequence.to_be_bytes());
    Ok(frame)
}

/// Builds a 12-byte downstream GPON `Assign_Alloc-ID` (`0x0a`) PLOAM frame
/// matching `fslstack_gpon_assign_alloc_id` (`alloc_type = 1` for GEM payload,
/// `0xff` to deallocate).
pub fn assign_alloc_id_ploam(
    onu_id: u8,
    alloc_id: u16,
    enable: bool,
    sequence: u16,
) -> io::Result<[u8; 12]> {
    if u32::from(alloc_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Alloc-ID exceeds 12-bit GPON range",
        ));
    }
    let mut frame = [0u8; 12];
    frame[0] = onu_id;
    frame[1] = PLOAM_MSG_ASSIGN_ALLOC_ID;
    frame[2] = (alloc_id >> 4) as u8;
    frame[3] = ((alloc_id & 0xf) << 4) as u8;
    frame[4] = if enable { 1 } else { 0xff };
    frame[5..7].copy_from_slice(&sequence.to_be_bytes());
    Ok(frame)
}

/// Builds a 12-byte downstream GPON `BER_Interval` (`0x12`) PLOAM frame
/// matching `fslstack_gpon_send_ber_interval_msg`.
pub fn ber_interval_ploam(onu_id: u8, interval: u32) -> [u8; 12] {
    let mut frame = [0u8; 12];
    frame[0] = onu_id;
    frame[1] = PLOAM_MSG_BER_INTERVAL;
    frame[2..6].copy_from_slice(&interval.to_be_bytes());
    frame
}

/// Converts a protocol ONU-ID to the 7-bit internal hardware `onu_index`
/// (`onu_id - offset`), matching `fsldrv_gpon_onuid_to_onuindex`.
pub fn onu_index(onu_id: u16, offset: u8) -> io::Result<u16> {
    let base = u16::from(offset);
    if onu_id < base {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU-ID is below configured port offset",
        ));
    }
    let idx = onu_id - base;
    if idx > MAX_ONU_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU index exceeds 7-bit hardware range",
        ));
    }
    Ok(idx)
}

fn rmw_onu_bitmap<B: Bus>(
    fsl: &mut Fsl<B>,
    reg_addr: u32,
    onu_index: u16,
    value: bool,
) -> io::Result<()> {
    if onu_index > MAX_ONU_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU index exceeds 128-bit active bitmap",
        ));
    }
    let mut words = fsl.read(RegisterSpace::Register, reg_addr, 0, ACTIVE_REG_WORDS)?;
    let word_idx = usize::from(onu_index >> 5);
    let bit_mask = 1u32 << (onu_index & 0x1f);
    if value {
        words[word_idx] |= bit_mask;
    } else {
        words[word_idx] &= !bit_mask;
    }
    fsl.write(RegisterSpace::Register, reg_addr, 0, &words)
}

/// Performs a 128-bit (4-word) RMW on `BMP_CFG_ONU_ACTIVE` (`0x602`) or
/// `ALM_PROC_ONU_ACTIVE_CFG` (`0x228`), preserving all neighboring ONU bits.
pub fn set_onu_active_bit<B: Bus>(
    fsl: &mut Fsl<B>,
    reg_addr: u32,
    onu_index: u16,
    active: bool,
) -> io::Result<()> {
    if reg_addr != BMP_CFG_ONU_ACTIVE && reg_addr != ALM_PROC_ONU_ACTIVE_CFG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported ONU active bitmap register",
        ));
    }
    rmw_onu_bitmap(fsl, reg_addr, onu_index, active)
}

/// Updates one ONU bit in `BMP_CFG_ONU_FEC_1G` (`0x633`)
/// (`fsldrv_gpon_bmp_cfg_onu_fec_set`).
pub fn set_onu_us_fec<B: Bus>(fsl: &mut Fsl<B>, onu_index: u16, enable: bool) -> io::Result<()> {
    rmw_onu_bitmap(fsl, BMP_CFG_ONU_FEC_1G, onu_index, enable)
}

/// Reads one ONU bit from `BMP_CFG_ONU_FEC_1G` (`0x633`).
pub fn get_onu_us_fec<B: Bus>(fsl: &mut Fsl<B>, onu_index: u16) -> io::Result<bool> {
    if onu_index > MAX_ONU_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU index exceeds 128-bit active bitmap",
        ));
    }
    let words = fsl.read(
        RegisterSpace::Register,
        BMP_CFG_ONU_FEC_1G,
        0,
        ACTIVE_REG_WORDS,
    )?;
    Ok((words[usize::from(onu_index >> 5)] >> (onu_index & 0x1f)) & 1 != 0)
}

/// Updates per-ONU upstream rate selection across `GPON_US_RATE_SEL_CFG`
/// (`0x212`) and `BMP_CFG_UP_RATE_IND` (`0x62f`)
/// (`fsldrv_gpon_us_rate_sel_cfg` + `fsldrv_gpon_bmp_cfg_up_rate_ind`).
pub fn set_onu_upstream_rate_2p5g<B: Bus>(
    fsl: &mut Fsl<B>,
    onu_index: u16,
    rate_2p5g: bool,
) -> io::Result<()> {
    rmw_onu_bitmap(fsl, GPON_US_RATE_SEL_CFG, onu_index, rate_2p5g)?;
    rmw_onu_bitmap(fsl, BMP_CFG_UP_RATE_IND, onu_index, rate_2p5g)
}

/// Sets or clears all bits in `BMP_CFG_PLOAMU_CTRL` (`0x637`)
/// (`fsldrv_gpon_bmp_set_force_ploamu_ind`).
pub fn set_force_ploamu<B: Bus>(fsl: &mut Fsl<B>, force_all: bool) -> io::Result<()> {
    let word = if force_all { u32::MAX } else { 0 };
    fsl.write(RegisterSpace::Register, BMP_CFG_PLOAMU_CTRL, 0, &[word; 8])
}

/// Programs message-ID filtering in `PLOAM_US_FILTER_CFG` (`0x458 + slot`)
/// (`fsldrv_gpon_common_us_ploam_filter_msgid_set`).
pub fn set_us_ploam_filter_msg_id<B: Bus>(
    fsl: &mut Fsl<B>,
    slot: u8,
    msg_id: u8,
    enable: bool,
) -> io::Result<()> {
    if slot >= MAX_PLOAM_FILTER_SLOTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PLOAM filter slot must be < 4",
        ));
    }
    let addr = PLOAM_US_FILTER_CFG + u32::from(slot);
    let mut reg = fsl.read_register(addr)?;
    reg = (reg & !((1 << 16) | 0xff)) | (u32::from(enable) << 16) | u32::from(msg_id);
    fsl.write(RegisterSpace::Register, addr, 0, &[reg])
}

/// Programs ONU-ID filtering in `PLOAM_US_FILTER_CFG` (`0x458 + slot`)
/// (`fsldrv_gpon_common_us_ploam_filter_onuid_set`).
pub fn set_us_ploam_filter_onu_id<B: Bus>(
    fsl: &mut Fsl<B>,
    slot: u8,
    onu_id: u8,
    enable: bool,
) -> io::Result<()> {
    if slot >= MAX_PLOAM_FILTER_SLOTS || onu_id > MAX_ONU_INDEX as u8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PLOAM filter slot or 7-bit ONU-ID out of range",
        ));
    }
    let addr = PLOAM_US_FILTER_CFG + u32::from(slot);
    let mut reg = fsl.read_register(addr)?;
    reg = (reg & !((1 << 15) | (0x7f << 8))) | (u32::from(enable) << 15) | (u32::from(onu_id) << 8);
    fsl.write(RegisterSpace::Register, addr, 0, &[reg])
}

/// Reads `ALLOCID_OFFSET` (bits `[4:3]`) from `BWMAP_CFG_BASIC` (`0x619`).
pub fn read_alloc_offset_mode<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u8> {
    let words = fsl.read(RegisterSpace::Register, BWMAP_CFG_BASIC, 0, 2)?;
    Ok(((words[0] >> 3) & 0x3) as u8)
}

// Alloc-ID → T-CONT mapping (`fsldrv_gpon_alloc_to_tcont`) lives once in
// `gdba::alloc_to_tcont`; this module only reads the offset mode that selects
// the carrier Alloc-ID window.

/// Triggers RSM T-CONT buffer flush via `RSM_CFG_TCONT_CLR` (`0x800`), polls for
/// completion under `policy`, and checks `RSM_ALM.TCONT_CLR_FAIL_ALM` (`0x810` bit 22)
/// (`fsldrv_gpon_rsm_clr_tcontid`).
pub fn clear_rsm_tcont<B: Bus>(
    fsl: &mut Fsl<B>,
    tcont_id: u8,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    checkpoint(Duration::ZERO)?;
    fsl.write(
        RegisterSpace::Register,
        RSM_CFG_TCONT_CLR,
        0,
        &[(u32::from(tcont_id) << 1) | 1],
    )?;
    polling::register(fsl, RSM_CFG_TCONT_CLR, 1, 0, policy, checkpoint)?;
    checkpoint(Duration::ZERO)?;
    let alm = fsl.read_register(RSM_ALM)?;
    if alm & RSM_ALM_TCONT_CLR_FAIL != 0 {
        return Err(io::Error::other("RSM reported T-CONT clear failure"));
    }
    Ok(())
}

/// Sends `Assign_ONU-ID` (`0x03`) PLOAMd frame `repeat` times (default stock 3)
/// through the single-owner `Management` session.
pub fn assign_onu_id<B: Bus>(
    mgmt: &mut Management<B>,
    onu_id: u8,
    serial_number: &[u8; 8],
    repeat: u8,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> Result<(), SendError> {
    let frame = assign_onu_id_ploam(onu_id, serial_number);
    for _ in 0..repeat {
        mgmt.send_ploam(&frame, policy, checkpoint)?;
    }
    Ok(())
}

/// Tears down an ONU's hardware datapath and active state: disables active/FEC
/// bitmaps, transmits `Deactivate_ONU-ID` (`0x05`), clears RSM T-CONTs, clears
/// GEM entries from `PORTID_CFG_TAB`, and clears alarm monitoring (`fslstack_gpon_remove_onu`).
#[allow(clippy::too_many_arguments)]
pub fn remove_onu<B: Bus>(
    mgmt: &mut Management<B>,
    onu_id: u8,
    onu_idx: u16,
    tcont_ids: &[u8],
    gem_port_ids: &[u16],
    tx_repeat: u8,
    tx_poll: PollPolicy,
    clear_poll: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> Result<(), SendError> {
    if let Err(err) = mgmt.ensure_ready() {
        return Err(SendError {
            submission: super::downstream::Submission::NotSubmitted,
            source: err,
        });
    }
    if let Err(err) = set_onu_active_bit(&mut mgmt.fsl, BMP_CFG_ONU_ACTIVE, onu_idx, false) {
        return Err(SendError {
            submission: super::downstream::Submission::NotSubmitted,
            source: err,
        });
    }
    if let Err(err) = set_onu_us_fec(&mut mgmt.fsl, onu_idx, false) {
        return Err(SendError {
            submission: super::downstream::Submission::NotSubmitted,
            source: err,
        });
    }
    let deact = deactivate_onu_ploam(onu_id);
    for _ in 0..tx_repeat {
        mgmt.send_ploam(&deact, tx_poll, checkpoint)?;
    }
    if let Err(err) = checkpoint(Duration::from_millis(1)) {
        return Err(SendError {
            submission: super::downstream::Submission::NotSubmitted,
            source: err,
        });
    }
    for &tcont in tcont_ids {
        if let Err(err) = clear_rsm_tcont(&mut mgmt.fsl, tcont, clear_poll, checkpoint) {
            return Err(SendError {
                submission: super::downstream::Submission::NotSubmitted,
                source: err,
            });
        }
    }
    for &gem in gem_port_ids {
        if let Err(err) = configure_gem_port(&mut mgmt.fsl, onu_idx, 0, gem, false) {
            return Err(SendError {
                submission: super::downstream::Submission::NotSubmitted,
                source: err,
            });
        }
    }
    if let Err(err) = set_onu_active_bit(&mut mgmt.fsl, ALM_PROC_ONU_ACTIVE_CFG, onu_idx, false) {
        return Err(SendError {
            submission: super::downstream::Submission::NotSubmitted,
            source: err,
        });
    }
    Ok(())
}

/// Updates one 8-bit slot in `PORTID_CFG_TAB` (`0x48a`), preserving the
/// other three GEM slots packed into the same 32-bit row.
pub fn configure_gem_port<B: Bus>(
    fsl: &mut Fsl<B>,
    onu_index: u16,
    flag: u8,
    gem_port_id: u16,
    enable: bool,
) -> io::Result<()> {
    if u32::from(gem_port_id) > MAX_ROW || onu_index > MAX_ONU_INDEX || flag > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid GEM Port-ID, ONU index, or flag",
        ));
    }
    let row = u32::from(gem_port_id >> 2);
    let shift = u32::from(gem_port_id & 0x3) * 8;
    let slot_mask = 0xffu32 << shift;
    let entry = if enable {
        u32::from((onu_index as u8 & 0x7f) | (flag << 7)) << shift
    } else {
        0
    };
    let mut words = fsl.read(RegisterSpace::Memory, PORTID_CFG_TAB, row, 1)?;
    words[0] = (words[0] & !slot_mask) | entry;
    fsl.write(RegisterSpace::Memory, PORTID_CFG_TAB, row, &words)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GemStatisDirection {
    Upstream,
    Downstream,
}

/// Triggers an OLT ranging grant request (`fsldrv_gpon_range_request`) via
/// `BMP_CFG_RG_REQ`, polls `BMP_STA.RANGE_ACK` under a bounded `PollPolicy`,
/// and pulses `BMP_PLS_REQ.RG_REQ` once acknowledged.
pub fn range_request<B: Bus>(
    fsl: &mut Fsl<B>,
    onu_index: u16,
    policy: PollPolicy,
    checkpoint: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    if onu_index > MAX_ONU_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU index exceeds 7-bit hardware range",
        ));
    }
    checkpoint(Duration::ZERO)?;
    // `RG_REQ_SEL` (bits 40:39) = 1, `RG_REQ_ONUID` (bits 38:32) = onu_index.
    let word1 = (1u32 << 7) | u32::from(onu_index);
    fsl.write(RegisterSpace::Register, BMP_CFG_RG_REQ, 0, &[0, word1])?;
    polling::register(
        fsl,
        BMP_STA,
        BMP_STA_RANGE_ACK,
        BMP_STA_RANGE_ACK,
        policy,
        checkpoint,
    )?;
    checkpoint(Duration::ZERO)?;
    fsl.write(RegisterSpace::Register, BMP_PLS_REQ, 0, &[BMP_PLS_RG_REQ])
}

/// Updates one GEM Port-ID's upstream WMCI (`[15:8]`) and AES (`[7:0]`) bits
/// in `GPON_UP_WMCI_AES_EN_CFG_TAB` (`0x4a7`), preserving the other 7 GEMs in
/// the same 16-bit row (`fsldrv_gpon_common_us_wmci_cfg`).
pub fn set_gem_wmci_aes<B: Bus>(
    fsl: &mut Fsl<B>,
    gem_port_id: u16,
    wmci_enable: bool,
    aes_enable: bool,
) -> io::Result<()> {
    if u32::from(gem_port_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GEM Port-ID exceeds 12-bit GPON range",
        ));
    }
    let row = u32::from(gem_port_id >> 3);
    let bit = u32::from(gem_port_id & 0x7);
    let aes_mask = 1u32 << bit;
    let wmci_mask = 1u32 << (bit + 8);
    let mut words = fsl.read(RegisterSpace::Memory, UP_WMCI_AES_EN_CFG_TAB, row, 1)?;
    let mut val = words[0] & !(aes_mask | wmci_mask);
    if aes_enable {
        val |= aes_mask;
    }
    if wmci_enable {
        val |= wmci_mask;
    }
    words[0] = val;
    fsl.write(RegisterSpace::Memory, UP_WMCI_AES_EN_CFG_TAB, row, &words)
}

/// Configures 9-bit per-GEM statistics enable (`bit 8`) and 8-bit counter
/// index (`[7:0]`) in `STATIS_US_GEM_EN_TAB` (`0x81a`) or
/// `STATIS_DS_GEM_EN_TAB` (`0x767`).
pub fn set_gem_statis_enable<B: Bus>(
    fsl: &mut Fsl<B>,
    direction: GemStatisDirection,
    gem_port_id: u16,
    enable: bool,
    counter_index: u8,
) -> io::Result<()> {
    if u32::from(gem_port_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GEM Port-ID exceeds 12-bit GPON range",
        ));
    }
    let addr = match direction {
        GemStatisDirection::Upstream => STATIS_US_GEM_EN_TAB,
        GemStatisDirection::Downstream => STATIS_DS_GEM_EN_TAB,
    };
    let val = (u32::from(enable) << 8) | u32::from(counter_index);
    fsl.write(RegisterSpace::Memory, addr, u32::from(gem_port_id), &[val])
}

/// Performs a 64-bit (2-word) RMW on `SWITCH_CFG_GEMPORT_BITMAP`
/// (`0xb01 + 2 * (gem_port_id >> 6)`), preserving all 63 neighboring GEM bits
/// (`fsldrv_gpon_common_switch_gemport_bitmap`).
pub fn set_switch_gemport_bitmap<B: Bus>(
    fsl: &mut Fsl<B>,
    gem_port_id: u16,
    enable: bool,
) -> io::Result<()> {
    if u32::from(gem_port_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GEM Port-ID exceeds 12-bit GPON range",
        ));
    }
    let bank = u32::from(gem_port_id >> 6);
    let addr = SWITCH_CFG_GEMPORT_BITMAP + bank * 2;
    let word_idx = usize::from((gem_port_id >> 5) & 1);
    let bit_mask = 1u32 << (gem_port_id & 0x1f);
    let mut words = fsl.read(RegisterSpace::Register, addr, 0, 2)?;
    if enable {
        words[word_idx] |= bit_mask;
    } else {
        words[word_idx] &= !bit_mask;
    }
    fsl.write(RegisterSpace::Register, addr, 0, &words)
}

#[cfg(test)]
mod tests;
