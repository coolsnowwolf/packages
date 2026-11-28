//! Stock GPON GDBA (T-CONT / OMCC bandwidth) table primitives.
//!
//! Recovered from `fsl_core_fttr_chip.ko` (`fsldrv_gpon_alloc_to_tcont`,
//! `fsldrv_gpon_gdba_ds_para_tab_set`, `fsldrv_gpon_gdba_para_l_h_tab_set`,
//! `fsldrv_gpon_gdba_na_num_set`, `fsldrv_gpon_gdba_alloc_info_conf`,
//! `fsldrv_gpon_gdba_pend_active_sence{1,2,3}_set`,
//! `fslstack_gpon_dba_all_bandwidth_config`, and
//! `fslstack_gpon_configure_omci_bw_and_port`).

use super::{
    activation::{ACTIVE_REG_WORDS, BMP_CFG_ONU_ACTIVE, BMP_CFG_ONU_FEC_1G, MAX_ONU_INDEX},
    initialization::defaults::{GDBA_SLOT_TAB, GPON_EXTEND_BURST_CFG, GPON_GUARD_TIME_CFG},
    protocol::{Bus, Fsl, MAX_ROW},
};
use crate::platform::diagnostics::RegisterSpace;
use std::{io, ops::Range};

/// `GDBA_WORK_CFG` (reg ID `0x7c`, 12-bit, base address `0x500`).
pub const GDBA_WORK_CFG: u32 = 0x500;
/// `GDBA_DS_TAB_REQ_CFG` (reg ID `0x7f`, 1-bit pulse, base address `0x505`).
pub const GDBA_DS_TAB_REQ_CFG: u32 = 0x505;
/// `GDBA_DS_SOFT_REQ_CFG` (reg ID `0x80`, 1-bit, base address `0x506`).
pub const GDBA_DS_SOFT_REQ_CFG: u32 = 0x506;
/// `GDBA_DS_TAB_STA` (reg ID `0x9f`, 2-bit, base address `0x50a`).
pub const GDBA_DS_TAB_STA: u32 = 0x50a;
/// `GDBA_DS_TAB_CFG` (reg ID `0x86`, 10-bit, base address `0x511`).
pub const GDBA_DS_TAB_CFG: u32 = 0x511;
/// `GDBA_NA_NUM_CFG` (reg ID `0x8c`, 9-bit, base address `0x51a`).
pub const GDBA_NA_NUM_CFG: u32 = 0x51a;
/// `GDBA_B_ALL_CFG` (reg ID `0x8f`, 58-bit / 2 words, base address `0x521`).
pub const GDBA_B_ALL_CFG: u32 = 0x521;
/// `GDBA_TAB_REQ_CFG` (reg ID `0x9a`, 1-bit pulse, base address `0x52f`).
pub const GDBA_TAB_REQ_CFG: u32 = 0x52f;

/// `GDBA_DS_PARA_TAB` (mem ID `0x1`, 256 x 17-bit rows, base address `0x539`).
pub const GDBA_DS_PARA_TAB: u32 = 0x539;
/// `GDBA_DS_ACTIVE_TAB` (mem ID `0x2`, 64 x 36-bit rows, base address `0x53a`).
pub const GDBA_DS_ACTIVE_TAB: u32 = 0x53a;
/// `GDBA_DS_PENDING_TAB` (mem ID `0x3`, 64 x 36-bit rows, base address `0x53b`).
pub const GDBA_DS_PENDING_TAB: u32 = 0x53b;
/// `GDBA_PARA_L_TAB` (mem ID `0x4`, 256 x 37-bit rows, base address `0x53c`).
pub const GDBA_PARA_L_TAB: u32 = 0x53c;
/// `GDBA_PARA_H_TAB` (mem ID `0x5`, 256 x 44-bit rows, base address `0x53d`).
pub const GDBA_PARA_H_TAB: u32 = 0x53d;

pub const MAX_BW_22BIT: u32 = 0x3f_ffff;
pub const MAX_NA_NUM: u16 = 0x1ff;
pub const MAX_DS_SLOT_ROWS: u16 = 64;
const DS_SLOT_BITS: u32 = 9;
const DS_SLOT_MASK: u64 = 0x1ff;
const DS_TAB_NUM_STOCK: u32 = 0x100;

/// GDBA downstream T-CONT slot layout controlled by `g_gdba_config_sence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GdbaScene {
    /// Scene `4` (`sence1`): up to 64 ONUs, 4 T-CONT slots in 1 row per ONU.
    FourPerOnu,
    /// Scene `2` (`sence2`, stock FTTR default): up to 128 ONUs, 2 slots per ONU
    /// packed into `onu_index >> 1` (low 2 slots for even, high 2 for odd).
    TwoPerOnu,
    /// Scene `8` (`sence3`): up to 32 ONUs, 8 T-CONT slots across 2 rows per ONU.
    EightPerOnu,
}

impl GdbaScene {
    /// Stock numeric scene selector passed to `fsldrv_gpon_gdba_sence_config`.
    pub const fn stock_code(self) -> u8 {
        match self {
            Self::FourPerOnu => 4,
            Self::TwoPerOnu => 2,
            Self::EightPerOnu => 8,
        }
    }

    /// Parses stock numeric scene selector (`2`, `4`, or `8`).
    pub fn from_stock_code(code: u8) -> io::Result<Self> {
        match code {
            4 => Ok(Self::FourPerOnu),
            2 => Ok(Self::TwoPerOnu),
            8 => Ok(Self::EightPerOnu),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported GDBA scene selector",
            )),
        }
    }

    fn row_targets(self, onu_index: u16) -> io::Result<Vec<(u32, Range<usize>)>> {
        match self {
            Self::FourPerOnu if onu_index < MAX_DS_SLOT_ROWS => {
                Ok(vec![(u32::from(onu_index), 0..4)])
            }
            Self::TwoPerOnu if onu_index < MAX_DS_SLOT_ROWS * 2 => {
                let start = usize::from((onu_index & 1) * 2);
                Ok(vec![(u32::from(onu_index >> 1), start..start + 2)])
            }
            Self::EightPerOnu if onu_index < MAX_DS_SLOT_ROWS / 2 => {
                let base = u32::from(onu_index) * 2;
                Ok(vec![(base, 0..4), (base + 1, 0..4)])
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONU index exceeds GDBA Active/Pending table capacity for scene",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GdbaAllocConfig {
    pub tcont: u8,
    pub onu_index: u16,
    pub enable: bool,
    pub fixed_bw: u32,
    pub assured_bw: u32,
    pub max_bw: u32,
    pub alloc_type: u8,
    pub priority: u8,
    pub weight: u8,
    pub ds_period: u8,
    pub ds_flag: bool,
    pub ds_bw_flag: bool,
    pub ds_tail_flag: bool,
}

impl GdbaAllocConfig {
    /// Builds the stock OMCC T-CONT allocation parameters used by
    /// `fslstack_gpon_configure_omci_bw_and_port`.
    pub fn omcc(tcont: u8, onu_index: u16, granularity_8x: u32, onu_type: u8) -> io::Result<Self> {
        if granularity_8x == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DBA granularity must be non-zero",
            ));
        }
        let mut bw = (granularity_8x + 0x1f3f) / granularity_8x;
        let is_type7 = onu_type == 7;
        if !is_type7 {
            bw = bw.saturating_mul(2);
        }
        if bw > MAX_BW_22BIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OMCC bandwidth exceeds 22-bit GDBA field",
            ));
        }
        Ok(Self {
            tcont,
            onu_index,
            enable: true,
            fixed_bw: bw,
            assured_bw: 0,
            max_bw: bw,
            alloc_type: 0,
            priority: 0,
            weight: 0x80,
            ds_period: 7,
            ds_flag: true,
            ds_bw_flag: false,
            ds_tail_flag: is_type7,
        })
    }

    /// Builds a business data T-CONT allocation matching stock
    /// `fslstack_gpon_addbw` / `fslstack_gpon_dba_config` bandwidth scaling.
    ///
    /// When `fixed_floor` is `None` or `Some(0)`, configures pure SR-DBA
    /// best-effort (`alloc_type = 2`, `fixed_bw = 0`, full line-rate `max_bw`).
    /// When a non-zero `fixed_floor` is requested (for ONUs needing a fixed
    /// grant floor), preserves that floor while keeping full line-rate `max_bw`.
    pub fn business_data(
        tcont: u8,
        onu_index: u16,
        granularity_8x: u32,
        is_2p5g: bool,
        fixed_floor: Option<u32>,
    ) -> io::Result<Self> {
        if granularity_8x == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DBA granularity must be non-zero",
            ));
        }
        let gran = u64::from(granularity_8x);
        let (scale, max_kbps) = if is_2p5g {
            // Live GSPON (2.488G) testing on AN4121-AM showed GDBA max_bw >= 215_000
            // triggers immediate LOSi/LOFi deactivations and 213_125 is marginal,
            // whereas 156_250 (1.25 Gbps cap at 8x granularity=64) stays alarm-free.
            (8u64, 1_250_000u64)
        } else {
            (16u64, 1_250_000u64)
        };
        let max_bw = (((gran - 1) + scale * max_kbps) / gran).min(u64::from(MAX_BW_22BIT)) as u32;
        let fixed_bw = fixed_floor.unwrap_or(0).min(max_bw);
        let alloc_type = if fixed_bw == 0 { 2 } else { 0 };
        Ok(Self {
            tcont,
            onu_index,
            enable: true,
            fixed_bw,
            assured_bw: 0,
            max_bw,
            alloc_type,
            priority: 0,
            weight: 0x80,
            ds_period: 7,
            ds_flag: true,
            ds_bw_flag: false,
            ds_tail_flag: is_2p5g,
        })
    }
}

/// Maps a 12-bit GPON `Alloc-ID` to the 8-bit hardware T-CONT index,
/// matching `fsldrv_gpon_alloc_to_tcont`.
pub fn alloc_to_tcont(
    alloc_id: u16,
    onu_offset: u8,
    max_onus: u8,
    alloc_offset_mode: u8,
) -> io::Result<u8> {
    if u32::from(alloc_id) > MAX_ROW {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Alloc-ID exceeds 12-bit GPON range",
        ));
    }
    let onu_base = u16::from(onu_offset);
    let max_onus_u16 = u16::from(max_onus);
    let omcc_end = onu_base.saturating_add(max_onus_u16);
    if alloc_id >= onu_base && alloc_id < omcc_end {
        let tcont = alloc_id - onu_base;
        return u8::try_from(tcont).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "OMCC T-CONT exceeds 8-bit table",
            )
        });
    }
    let data_base: u16 = match alloc_offset_mode {
        0 => 0x400,
        1 => 0x800,
        2 => 0xc00,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported GDBA Alloc-ID offset mode",
            ));
        }
    };
    let data_end = data_base + 0x100 - max_onus_u16;
    if alloc_id >= data_base && alloc_id < data_end {
        return Ok(((alloc_id - data_base) + max_onus_u16) as u8);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Alloc-ID outside configured OMCC/T-CONT bands",
    ))
}

/// Computes the downstream period clamp from `GDBA_WORK_CFG.DBA_CALC_CYCLE`.
pub fn calc_cycle_mask(dba_calc_cycle: u8) -> u8 {
    if dba_calc_cycle > 7 {
        return 0;
    }
    let shift = 7 - dba_calc_cycle;
    (((1u16 << shift) - 1).min(0x3f)) as u8
}

/// Writes `GDBA_DS_PARA_TAB` (`0x539`) and triggers `GDBA_DS_TAB_REQ_CFG` (`0x505`),
/// then writes `GDBA_PARA_H_TAB` (`0x53d`), `GDBA_PARA_L_TAB` (`0x53c`), and
/// triggers `GDBA_TAB_REQ_CFG` (`0x52f`).
pub fn configure_alloc<B: Bus>(fsl: &mut Fsl<B>, cfg: &GdbaAllocConfig) -> io::Result<()> {
    if cfg.onu_index > MAX_ONU_INDEX
        || cfg.fixed_bw > MAX_BW_22BIT
        || cfg.assured_bw > MAX_BW_22BIT
        || cfg.max_bw > MAX_BW_22BIT
        || cfg.alloc_type > 3
        || cfg.priority > 7
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDBA allocation parameter exceeds hardware field bounds",
        ));
    }
    let work_cfg = fsl.read_register(GDBA_WORK_CFG)?;
    let calc_cycle = ((work_cfg >> 1) & 0x7) as u8;
    let period_cap = calc_cycle_mask(calc_cycle);
    let row = u32::from(cfg.tcont);

    let ds_word = if cfg.enable {
        let period = cfg.ds_period.min(period_cap) & 0x3f;
        (1u32 << 16)
            | (u32::from(cfg.onu_index & 0x7f) << 9)
            | (u32::from(cfg.ds_bw_flag) << 8)
            | (u32::from(cfg.ds_flag) << 7)
            | (u32::from(period) << 1)
            | u32::from(cfg.ds_tail_flag)
    } else {
        0
    };
    fsl.write(RegisterSpace::Memory, GDBA_DS_PARA_TAB, row, &[ds_word])?;
    fsl.write(RegisterSpace::Register, GDBA_DS_TAB_REQ_CFG, 0, &[1])?;

    if cfg.enable {
        let para_h: u64 = (u64::from(cfg.fixed_bw) << 22) | u64::from(cfg.assured_bw);
        let para_l: u64 = (1u64 << 36)
            | (u64::from(cfg.max_bw) << 14)
            | (u64::from(cfg.alloc_type) << 11)
            | (u64::from(cfg.priority) << 8)
            | u64::from(cfg.weight);
        fsl.write(
            RegisterSpace::Memory,
            GDBA_PARA_H_TAB,
            row,
            &[para_h as u32, (para_h >> 32) as u32],
        )?;
        fsl.write(
            RegisterSpace::Memory,
            GDBA_PARA_L_TAB,
            row,
            &[para_l as u32, (para_l >> 32) as u32],
        )?;
    } else {
        fsl.write(RegisterSpace::Memory, GDBA_PARA_L_TAB, row, &[0, 0])?;
        fsl.write(RegisterSpace::Memory, GDBA_PARA_H_TAB, row, &[0, 0])?;
    }
    fsl.write(RegisterSpace::Register, GDBA_TAB_REQ_CFG, 0, &[1])
}

/// Updates `GDBA_NA_NUM_CFG` (`0x51a`, 9-bit active non-assured T-CONT count).
pub fn set_na_num<B: Bus>(fsl: &mut Fsl<B>, count: u16) -> io::Result<()> {
    if count > MAX_NA_NUM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDBA NA count exceeds 9-bit hardware range",
        ));
    }
    fsl.write(
        RegisterSpace::Register,
        GDBA_NA_NUM_CFG,
        0,
        &[u32::from(count)],
    )
}

fn update_ds_slot_rows(
    rows: &mut [u64],
    targets: &[(u32, Range<usize>)],
    tcont: u8,
    enable: bool,
) -> io::Result<()> {
    let target = u64::from(tcont);
    if enable {
        for (row_val, (_, range)) in rows.iter_mut().zip(targets) {
            for slot in range.clone() {
                let shift = slot as u32 * DS_SLOT_BITS;
                if (*row_val >> shift) & 0xff == target {
                    *row_val |= 1u64 << (shift + 8);
                    return Ok(());
                }
            }
        }
        for (row_val, (_, range)) in rows.iter_mut().zip(targets) {
            for slot in range.clone() {
                let shift = slot as u32 * DS_SLOT_BITS;
                if (*row_val >> (shift + 8)) & 1 == 0 {
                    *row_val = (*row_val & !(DS_SLOT_MASK << shift)) | ((0x100 | target) << shift);
                    return Ok(());
                }
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no free GDBA Active/Pending downstream slot for ONU",
        ));
    }

    for (row_val, (_, range)) in rows.iter_mut().zip(targets) {
        for slot in range.clone() {
            let shift = slot as u32 * DS_SLOT_BITS;
            if (*row_val >> shift) & 0xff == target {
                *row_val &= !(DS_SLOT_MASK << shift);
                return Ok(());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "T-CONT index not found in GDBA Active/Pending downstream slots",
    ))
}

fn read_ds_rows<B: Bus>(
    fsl: &mut Fsl<B>,
    table: u32,
    targets: &[(u32, Range<usize>)],
) -> io::Result<Vec<u64>> {
    let mut rows = Vec::with_capacity(targets.len());
    for &(row, _) in targets {
        let words = fsl.read(RegisterSpace::Memory, table, row, 2)?;
        let value = u64::from(words[0]) | (u64::from(words[1] & 0xf) << 32);
        rows.push(value);
    }
    Ok(rows)
}

fn write_ds_rows<B: Bus>(
    fsl: &mut Fsl<B>,
    table: u32,
    targets: &[(u32, Range<usize>)],
    rows: &[u64],
) -> io::Result<()> {
    for (&(row, _), &value) in targets.iter().zip(rows) {
        fsl.write(
            RegisterSpace::Memory,
            table,
            row,
            &[value as u32, ((value >> 32) as u32) & 0xf],
        )?;
    }
    Ok(())
}

/// Updates `GDBA_DS_ACTIVE_TAB` (`0x53a`) and `GDBA_DS_PENDING_TAB` (`0x53b`)
/// using the stock two-table soft-swap sequence (`fsldrv_gpon_gdba_pend_active_sence{1,2,3}_set`).
///
/// `pending_initialized` tracks `s_gdba_na_tcont[pon + 256]`: on first use it
/// forces the Pending path even when `DS_TAB_SOFT_SEL_STA` reads `1`.
pub fn configure_ds_active_pending_slot<B: Bus>(
    fsl: &mut Fsl<B>,
    scene: GdbaScene,
    onu_index: u16,
    tcont: u8,
    enable: bool,
    pending_initialized: &mut bool,
) -> io::Result<()> {
    let targets = scene.row_targets(onu_index)?;
    let status = fsl.read_register(GDBA_DS_TAB_STA)?;
    let sel_active = ((status >> 1) & 1) == 1 && *pending_initialized;
    *pending_initialized = true;

    let (primary, secondary, req_sel) = if sel_active {
        (GDBA_DS_ACTIVE_TAB, GDBA_DS_PENDING_TAB, 1u32)
    } else {
        (GDBA_DS_PENDING_TAB, GDBA_DS_ACTIVE_TAB, 0u32)
    };

    let mut rows = read_ds_rows(fsl, primary, &targets)?;
    update_ds_slot_rows(&mut rows, &targets, tcont, enable)?;
    write_ds_rows(fsl, primary, &targets, &rows)?;
    fsl.write(RegisterSpace::Register, GDBA_DS_SOFT_REQ_CFG, 0, &[1])?;
    fsl.write(
        RegisterSpace::Register,
        GDBA_DS_TAB_CFG,
        0,
        &[(req_sel << 9) | DS_TAB_NUM_STOCK],
    )?;
    write_ds_rows(fsl, secondary, &targets, &rows)
}

/// Computes `B_ALL_SET_THD` / `B_ALL_SET` per `fslstack_gpon_dba_all_bandwidth_config`.
pub fn compute_b_all_set_thd(
    base_thd: u16,
    active_onus: u16,
    total_gap: u32,
    any_onu_fec: bool,
) -> u16 {
    let gap = total_gap >> 2;
    let base = u32::from(base_thd);
    if base > 16 * u32::from(active_onus) && gap <= base {
        let mut value = base - gap;
        if any_onu_fec {
            value = (239 * value) / 0xff;
        }
        value as u16
    } else {
        0
    }
}

/// Updates `GDBA_B_ALL_CFG` (`0x521`) fields `B_ALL_SET_THD` (bits 47:32) and
/// `B_ALL_SET` (bits 15:0), preserving `B_ALL_MODE` (bit 57) and `ALLOC_IFIFO_NUM_THR` (bits 56:48).
pub fn write_b_all_set_thd<B: Bus>(fsl: &mut Fsl<B>, value: u16) -> io::Result<()> {
    let mut words = fsl.read(RegisterSpace::Register, GDBA_B_ALL_CFG, 0, 2)?;
    let v = u32::from(value);
    words[0] = (words[0] & !0xffff) | v;
    words[1] = (words[1] & !0xffff) | v;
    fsl.write(RegisterSpace::Register, GDBA_B_ALL_CFG, 0, &words)
}

/// Optional parameters governing `fslstack_gpon_cur_all_gap_get`.
/// Defaults match `fslstack_gpon_dba_adjust_param_default_init` + stock `g_gpon_dba_send_cycle = 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GdbaGapConfig {
    pub multi_grant_enabled: bool,
    pub multi_grant_rate: u32,
    pub multi_grant_shift: u8,
    pub dba_send_cycle: u8,
    pub max_onus: u8,
}

impl Default for GdbaGapConfig {
    fn default() -> Self {
        Self {
            multi_grant_enabled: false,
            multi_grant_rate: 100_000,
            multi_grant_shift: 5,
            dba_send_cycle: 0,
            max_onus: 128,
        }
    }
}

/// Snapshot of dynamic GDBA gap inputs read from hardware (`fslstack_gpon_cur_all_gap_get`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GdbaGapSummary {
    pub guard_time_len: u8,
    pub type3_o5o6_len: u8,
    pub dba_calc_cycle: u8,
    pub active_onus: u16,
    pub total_gap: u32,
}

fn validate_max_onus(max_onus: u8) -> io::Result<()> {
    if u16::from(max_onus) > MAX_ONU_INDEX + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_onus exceeds 128-bit ONU bitmap capacity",
        ));
    }
    Ok(())
}

/// Reads `GPON_GUARD_TIME_CFG.GUARD_TIME_LEN_CFG` (`0x207` bits `[7:0]`).
pub fn read_guard_time_len<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u8> {
    Ok((fsl.read_register(GPON_GUARD_TIME_CFG)? & 0xff) as u8)
}

/// Reads `GPON_EXTEND_BURST_CFG.TYPE3_O5O6_LEN_CFG` (`0x206` bits `[15:8]`).
pub fn read_type3_o5o6_len<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u8> {
    Ok(((fsl.read_register(GPON_EXTEND_BURST_CFG)? >> 8) & 0xff) as u8)
}

/// Decodes effective DBA calculation cycle from `GDBA_WORK_CFG` (`0x500` bits `[3:1]`)
/// and `GDBA_SLOT_TAB` (`0x504` mem row 0 bits `[5:1]`) (`fsldrv_gpon_gdba_dba_calc_cycle_get`).
pub fn read_dba_calc_cycle<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u8> {
    let work_cfg = fsl.read_register(GDBA_WORK_CFG)?;
    let work_cycle = ((work_cfg >> 1) & 0x7) as u8;
    let slot_words = fsl.read(RegisterSpace::Memory, GDBA_SLOT_TAB, 0, 1)?;
    let slot_bits = ((slot_words[0] >> 1) & 0x1f) as u8;
    match (work_cycle, slot_bits) {
        (0, 1) => Ok(0),
        (0, 3) => Ok(1),
        (0, 7) => Ok(2),
        (0, 15) => Ok(3),
        (w, 31) => Ok(w.saturating_add(4)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported GDBA_WORK_CFG / GDBA_SLOT_TAB calc cycle state",
        )),
    }
}

/// Computes `8 * dba_granularity = 1024 >> dba_calc_cycle` (`fsldrv_gpon_8_times_dba_granularity_get`).
pub fn read_eight_times_dba_granularity<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u32> {
    let cycle = read_dba_calc_cycle(fsl)?;
    if cycle > 7 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DBA calc cycle exceeds valid range 0..=7",
        ));
    }
    Ok(1024 >> cycle)
}

/// Reads `GDBA_B_ALL_CFG.B_ALL_MODE` (`0x521` bit 57) (`fsldrv_gpon_gdba_b_all_mode_get`).
pub fn read_b_all_mode<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<bool> {
    let words = fsl.read(RegisterSpace::Register, GDBA_B_ALL_CFG, 0, 2)?;
    Ok(((words[1] >> 25) & 1) != 0)
}

/// Reads `GDBA_B_ALL_CFG.B_ALL_SET_THD` (`0x521` bits `[47:32]`) (`fsldrv_gpon_gdba_b_all_set_thd_get`).
pub fn read_b_all_set_thd<B: Bus>(fsl: &mut Fsl<B>) -> io::Result<u16> {
    let words = fsl.read(RegisterSpace::Register, GDBA_B_ALL_CFG, 0, 2)?;
    Ok((words[1] & 0xffff) as u16)
}

fn mask_bitmap_bits(words: &mut [u32; ACTIVE_REG_WORDS], max_onus: u8) {
    let mut rem = u16::from(max_onus);
    for w in words.iter_mut() {
        if rem == 0 {
            *w = 0;
        } else if rem < 32 {
            *w &= (1u32 << rem) - 1;
            rem = 0;
        } else {
            rem -= 32;
        }
    }
}

/// Reads `BMP_CFG_ONU_ACTIVE` (`0x602`) and counts active ONUs within `0..max_onus`.
pub fn read_active_onu_count<B: Bus>(fsl: &mut Fsl<B>, max_onus: u8) -> io::Result<u16> {
    validate_max_onus(max_onus)?;
    if max_onus == 0 {
        return Ok(0);
    }
    let raw = fsl.read(
        RegisterSpace::Register,
        BMP_CFG_ONU_ACTIVE,
        0,
        ACTIVE_REG_WORDS,
    )?;
    let mut words: [u32; ACTIVE_REG_WORDS] = raw.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected BMP_CFG_ONU_ACTIVE word count",
        )
    })?;
    mask_bitmap_bits(&mut words, max_onus);
    Ok(words.into_iter().map(|w| w.count_ones() as u16).sum())
}

/// Reads the per-ONU upstream FEC enable bitmap from `BMP_CFG_ONU_FEC_1G` (`0x633`).
pub fn read_onu_fec_bitmap<B: Bus>(fsl: &mut Fsl<B>, max_onus: u8) -> io::Result<Vec<bool>> {
    validate_max_onus(max_onus)?;
    if max_onus == 0 {
        return Ok(Vec::new());
    }
    let fec = fsl.read(
        RegisterSpace::Register,
        BMP_CFG_ONU_FEC_1G,
        0,
        ACTIVE_REG_WORDS,
    )?;
    let mut fec_words: [u32; ACTIVE_REG_WORDS] = fec.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected BMP_CFG_ONU_FEC_1G word count",
        )
    })?;
    mask_bitmap_bits(&mut fec_words, max_onus);
    Ok((0..usize::from(max_onus))
        .map(|idx| ((fec_words[idx / 32] >> (idx % 32)) & 1) != 0)
        .collect())
}

/// Returns `true` if any active ONU in `0..max_onus` has upstream FEC enabled in `BMP_CFG_ONU_FEC_1G` (`0x633`),
/// matching `fslstack_gpon_onu_fec_state_get`.
pub fn any_active_onu_us_fec<B: Bus>(fsl: &mut Fsl<B>, max_onus: u8) -> io::Result<bool> {
    validate_max_onus(max_onus)?;
    if max_onus == 0 {
        return Ok(false);
    }
    let active = fsl.read(
        RegisterSpace::Register,
        BMP_CFG_ONU_ACTIVE,
        0,
        ACTIVE_REG_WORDS,
    )?;
    let fec = fsl.read(
        RegisterSpace::Register,
        BMP_CFG_ONU_FEC_1G,
        0,
        ACTIVE_REG_WORDS,
    )?;
    let mut active_words: [u32; ACTIVE_REG_WORDS] = active.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected BMP_CFG_ONU_ACTIVE word count",
        )
    })?;
    let mut fec_words: [u32; ACTIVE_REG_WORDS] = fec.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected BMP_CFG_ONU_FEC_1G word count",
        )
    })?;
    mask_bitmap_bits(&mut active_words, max_onus);
    mask_bitmap_bits(&mut fec_words, max_onus);
    Ok(active_words
        .into_iter()
        .zip(fec_words)
        .any(|(a, f)| (a & f) != 0))
}

/// Reads guard time, Type-3 O5/O6 burst length, DBA calculation cycle, and active ONU
/// count to compute `total_gap`, matching `fslstack_gpon_cur_all_gap_get`.
pub fn read_cur_all_gap<B: Bus>(
    fsl: &mut Fsl<B>,
    cfg: &GdbaGapConfig,
) -> io::Result<GdbaGapSummary> {
    validate_max_onus(cfg.max_onus)?;
    let guard_time_len = read_guard_time_len(fsl)?;
    let type3_o5o6_len = read_type3_o5o6_len(fsl)?;
    let dba_calc_cycle = read_dba_calc_cycle(fsl)?;
    let active_onus = read_active_onu_count(fsl, cfg.max_onus)?;

    let mut unit_gap = 8u32 * (u32::from(guard_time_len) + u32::from(type3_o5o6_len) + 6);
    if dba_calc_cycle > 4 {
        unit_gap >>= (dba_calc_cycle - 4).min(31);
    }
    let total_gap = if cfg.multi_grant_enabled {
        let rate = cfg.multi_grant_rate.max(1);
        let threshold = 0x12_fc00 / rate;
        let active = u32::from(active_onus);
        if threshold > active {
            unit_gap.saturating_mul(active)
        } else {
            let head = unit_gap.saturating_mul(threshold);
            let tail =
                (unit_gap >> cfg.multi_grant_shift.min(31)).saturating_mul(active - threshold);
            head.saturating_add(tail)
        }
    } else {
        (unit_gap >> cfg.dba_send_cycle.min(31)).saturating_mul(u32::from(active_onus))
    };

    Ok(GdbaGapSummary {
        guard_time_len,
        type3_o5o6_len,
        dba_calc_cycle,
        active_onus,
        total_gap,
    })
}

/// Dynamically recalculates and writes `B_ALL_SET_THD` / `B_ALL_SET` in
/// `GDBA_B_ALL_CFG` (`0x521`) when `B_ALL_MODE` is enabled, matching
/// `fslstack_gpon_dba_all_bandwidth_config`. Returns `Ok(None)` when `B_ALL_MODE == 0`.
pub fn recalculate_b_all_bandwidth<B: Bus>(
    fsl: &mut Fsl<B>,
    base_thd: u16,
    cfg: &GdbaGapConfig,
) -> io::Result<Option<u16>> {
    if !read_b_all_mode(fsl)? {
        return Ok(None);
    }
    let gap = read_cur_all_gap(fsl, cfg)?;
    let any_fec = any_active_onu_us_fec(fsl, cfg.max_onus)?;
    let new_thd = compute_b_all_set_thd(base_thd, gap.active_onus, gap.total_gap, any_fec);
    write_b_all_set_thd(fsl, new_thd)?;
    Ok(Some(new_thd))
}

#[cfg(test)]
mod tests;
