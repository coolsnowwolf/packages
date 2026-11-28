use super::*;
use crate::platform::fiberhome_an4475_g2::test_support::{
    message, read, write, Mock, Script, Step,
};
use std::{cell::RefCell, rc::Rc};

fn mock_fsl(steps: Vec<Step>) -> (Fsl<Mock>, Mock) {
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: steps.into(),
        ..Script::default()
    })));
    (Fsl::new(bus.clone()), bus)
}

fn write_mem(address: u32, row: u32, words: &[u32]) -> Step {
    let mut data = Vec::with_capacity(words.len() * 4);
    for word in words.iter().rev() {
        data.extend_from_slice(&word.to_be_bytes());
    }
    message(
        [
            ((address >> 8) as u8) << 3 | 5,
            address as u8,
            (((words.len() - 1) as u8) << 4) | (row >> 8) as u8,
            row as u8,
            0,
        ],
        &data,
        vec![],
    )
}

fn read_mem(address: u32, row: u32, words: &[u32]) -> Step {
    let mut rx = Vec::with_capacity(words.len() * 4);
    for word in words.iter().rev() {
        rx.extend_from_slice(&word.to_be_bytes());
    }
    message(
        [
            ((address >> 8) as u8) << 3 | 6,
            address as u8,
            (((words.len() - 1) as u8) << 4) | (row >> 8) as u8,
            row as u8,
            0,
        ],
        &[0],
        rx,
    )
}

fn read_reg_words(address: u32, words: &[u32]) -> Step {
    let mut rx = Vec::with_capacity(words.len() * 4);
    for word in words {
        rx.extend_from_slice(&word.to_be_bytes());
    }
    message(
        [
            ((address >> 8) as u8) << 3 | 2,
            address as u8,
            ((words.len() - 1) as u8) << 4,
            0,
            0,
        ],
        &[0],
        rx,
    )
}

#[test]
fn alloc_to_tcont_handles_omcc_and_data_bands() {
    assert_eq!(alloc_to_tcont(5, 0, 128, 0).unwrap(), 5);
    assert_eq!(alloc_to_tcont(15, 10, 64, 0).unwrap(), 5);
    assert_eq!(alloc_to_tcont(0x400, 0, 128, 0).unwrap(), 128);
    assert_eq!(alloc_to_tcont(0x47f, 0, 128, 0).unwrap(), 255);
    assert_eq!(alloc_to_tcont(0x800, 0, 128, 1).unwrap(), 128);
    assert_eq!(alloc_to_tcont(2050, 0, 128, 1).unwrap(), 130);
    assert_eq!(alloc_to_tcont(0xc00, 0, 128, 2).unwrap(), 128);
    assert_eq!(alloc_to_tcont(3080, 0, 128, 2).unwrap(), 136);

    // Default T-CONT window with a non-zero hardware ONU-ID offset.
    assert_eq!(alloc_to_tcont(10, 4, 128, 0).unwrap(), 6);
    assert!(alloc_to_tcont(3, 4, 128, 0).is_err());

    assert!(alloc_to_tcont(0x480, 0, 128, 0).is_err());
    assert!(alloc_to_tcont(0x400, 0, 128, 1).is_err());
    assert!(alloc_to_tcont(0x1000, 0, 128, 0).is_err());
    assert!(alloc_to_tcont(0x400, 0, 128, 3).is_err());
}

#[test]
fn omcc_alloc_config_matches_type7_and_non_type7_stock_math() {
    // 8000 / 1000 = 8 -> +0x1f3f rounds up to 8 for granularity=1000.
    let t7 = GdbaAllocConfig::omcc(3, 3, 1000, 7).unwrap();
    assert_eq!(t7.fixed_bw, 8);
    assert_eq!(t7.max_bw, 8);
    assert!(t7.ds_tail_flag);

    let t3 = GdbaAllocConfig::omcc(3, 3, 1000, 3).unwrap();
    assert_eq!(t3.fixed_bw, 16);
    assert_eq!(t3.max_bw, 16);
    assert!(!t3.ds_tail_flag);

    assert!(GdbaAllocConfig::omcc(3, 3, 0, 7).is_err());
}

#[test]
fn business_data_alloc_config_scales_best_effort_and_floor_modes() {
    let be_1g = GdbaAllocConfig::business_data(128, 0, 64, false, None).unwrap();
    assert_eq!(be_1g.fixed_bw, 0);
    assert_eq!(be_1g.max_bw, 312_500);
    assert_eq!(be_1g.alloc_type, 2);
    assert!(be_1g.ds_flag);
    assert!(!be_1g.ds_tail_flag);

    let floor_1g = GdbaAllocConfig::business_data(129, 1, 64, false, Some(1300)).unwrap();
    assert_eq!(floor_1g.fixed_bw, 1300);
    assert_eq!(floor_1g.max_bw, 312_500);
    assert_eq!(floor_1g.alloc_type, 0);
    assert!(floor_1g.ds_flag);

    let be_2p5g = GdbaAllocConfig::business_data(130, 2, 64, true, Some(0)).unwrap();
    assert_eq!(be_2p5g.fixed_bw, 0);
    assert_eq!(be_2p5g.max_bw, 156_250);
    assert_eq!(be_2p5g.alloc_type, 2);
    assert!(be_2p5g.ds_tail_flag);

    assert!(GdbaAllocConfig::business_data(128, 0, 0, false, None).is_err());
}

#[test]
fn configure_alloc_writes_ds_para_h_l_and_request_pulses() {
    let cfg = GdbaAllocConfig {
        tcont: 0x12,
        onu_index: 5,
        enable: true,
        fixed_bw: 8,
        assured_bw: 1,
        max_bw: 16,
        alloc_type: 2,
        priority: 3,
        weight: 0x80,
        ds_period: 7,
        ds_flag: true,
        ds_bw_flag: false,
        ds_tail_flag: true,
    };
    // DBA_CALC_CYCLE = 6 (bits 3:1 = 6 => val = 0xc) -> mask = (1 << 1) - 1 = 1,
    // clamping ds_period=7 to 1.
    let expected_ds = (1 << 16) | (5 << 9) | (1 << 7) | (1 << 1) | 1;
    let expected_h: u64 = (8u64 << 22) | 1;
    let expected_l: u64 = (1u64 << 36) | (16u64 << 14) | (2u64 << 11) | (3u64 << 8) | 0x80;

    let (mut fsl, bus) = mock_fsl(vec![
        read(GDBA_WORK_CFG, 0x0c),
        write_mem(GDBA_DS_PARA_TAB, 0x12, &[expected_ds]),
        write(GDBA_DS_TAB_REQ_CFG, &[1]),
        write_mem(
            GDBA_PARA_H_TAB,
            0x12,
            &[expected_h as u32, (expected_h >> 32) as u32],
        ),
        write_mem(
            GDBA_PARA_L_TAB,
            0x12,
            &[expected_l as u32, (expected_l >> 32) as u32],
        ),
        write(GDBA_TAB_REQ_CFG, &[1]),
    ]);
    configure_alloc(&mut fsl, &cfg).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    let disable = GdbaAllocConfig {
        enable: false,
        ..cfg
    };
    let (mut fsl, bus) = mock_fsl(vec![
        read(GDBA_WORK_CFG, 0),
        write_mem(GDBA_DS_PARA_TAB, 0x12, &[0]),
        write(GDBA_DS_TAB_REQ_CFG, &[1]),
        write_mem(GDBA_PARA_L_TAB, 0x12, &[0, 0]),
        write_mem(GDBA_PARA_H_TAB, 0x12, &[0, 0]),
        write(GDBA_TAB_REQ_CFG, &[1]),
    ]);
    configure_alloc(&mut fsl, &disable).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    let bad = GdbaAllocConfig {
        fixed_bw: MAX_BW_22BIT + 1,
        ..cfg
    };
    assert!(configure_alloc(&mut fsl, &bad).is_err());
}

#[test]
fn set_na_num_validates_9bit_bound() {
    let (mut fsl, bus) = mock_fsl(vec![write(GDBA_NA_NUM_CFG, &[0x1ff])]);
    set_na_num(&mut fsl, 0x1ff).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(set_na_num(&mut fsl, 0x200).is_err());
}

#[test]
fn ds_active_pending_sence2_first_init_uses_pending_then_active_and_packs_36bit_row() {
    assert_eq!(GdbaScene::from_stock_code(2).unwrap(), GdbaScene::TwoPerOnu);
    assert_eq!(
        GdbaScene::from_stock_code(4).unwrap(),
        GdbaScene::FourPerOnu
    );
    assert_eq!(
        GdbaScene::from_stock_code(8).unwrap(),
        GdbaScene::EightPerOnu
    );
    assert!(GdbaScene::from_stock_code(1).is_err());

    let mut pending_initialized = false;
    // Even if DS_TAB_SOFT_SEL_STA == 1 (val = 2), first config forces Pending path (0x53b -> 0x511=0x100 -> 0x53a).
    // Row 0 starts with ONU0 slot0 occupied by tcont 0x05 valid (0x105). Enable ONU1 tcont 0x81 in slot2 (bits 26:18)
    // and then enable ONU1 tcont 0x82 in slot3 (bits 35:27 -> crosses into bit35 of word[1] = 0x8!).
    let initial_row = 0x105u64;
    let after_slot2 = initial_row | ((0x100 | 0x81u64) << 18);
    let after_slot3 = after_slot2 | ((0x100 | 0x82u64) << 27);
    let after_disable = after_slot3 & !(DS_SLOT_MASK << 18);

    let (mut fsl, bus) = mock_fsl(vec![
        // 1) First call on odd ONU 1 -> row 0, forced Pending primary.
        read(GDBA_DS_TAB_STA, 2),
        read_mem(
            GDBA_DS_PENDING_TAB,
            0,
            &[initial_row as u32, (initial_row >> 32) as u32],
        ),
        write_mem(
            GDBA_DS_PENDING_TAB,
            0,
            &[after_slot2 as u32, (after_slot2 >> 32) as u32],
        ),
        write(GDBA_DS_SOFT_REQ_CFG, &[1]),
        write(GDBA_DS_TAB_CFG, &[0x100]),
        write_mem(
            GDBA_DS_ACTIVE_TAB,
            0,
            &[after_slot2 as u32, (after_slot2 >> 32) as u32],
        ),
        // 2) Second call with DS_TAB_SOFT_SEL_STA == 1 -> uses Active primary (req_sel = 1 => 0x300).
        read(GDBA_DS_TAB_STA, 2),
        read_mem(
            GDBA_DS_ACTIVE_TAB,
            0,
            &[after_slot2 as u32, (after_slot2 >> 32) as u32],
        ),
        write_mem(
            GDBA_DS_ACTIVE_TAB,
            0,
            &[after_slot3 as u32, (after_slot3 >> 32) as u32],
        ),
        write(GDBA_DS_SOFT_REQ_CFG, &[1]),
        write(GDBA_DS_TAB_CFG, &[0x300]),
        write_mem(
            GDBA_DS_PENDING_TAB,
            0,
            &[after_slot3 as u32, (after_slot3 >> 32) as u32],
        ),
        // 3) Disable tcont 0x81 on ONU 1 -> clears slot2 while leaving ONU0 slot0 and ONU1 slot3 intact.
        read(GDBA_DS_TAB_STA, 0),
        read_mem(
            GDBA_DS_PENDING_TAB,
            0,
            &[after_slot3 as u32, (after_slot3 >> 32) as u32],
        ),
        write_mem(
            GDBA_DS_PENDING_TAB,
            0,
            &[after_disable as u32, (after_disable >> 32) as u32],
        ),
        write(GDBA_DS_SOFT_REQ_CFG, &[1]),
        write(GDBA_DS_TAB_CFG, &[0x100]),
        write_mem(
            GDBA_DS_ACTIVE_TAB,
            0,
            &[after_disable as u32, (after_disable >> 32) as u32],
        ),
    ]);

    configure_ds_active_pending_slot(
        &mut fsl,
        GdbaScene::TwoPerOnu,
        1,
        0x81,
        true,
        &mut pending_initialized,
    )
    .unwrap();
    assert!(pending_initialized);
    configure_ds_active_pending_slot(
        &mut fsl,
        GdbaScene::TwoPerOnu,
        1,
        0x82,
        true,
        &mut pending_initialized,
    )
    .unwrap();
    configure_ds_active_pending_slot(
        &mut fsl,
        GdbaScene::TwoPerOnu,
        1,
        0x81,
        false,
        &mut pending_initialized,
    )
    .unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    // ONU 1 full error when both slots are occupied.
    let (mut fsl, _) = mock_fsl(vec![
        read(GDBA_DS_TAB_STA, 2),
        read_mem(
            GDBA_DS_ACTIVE_TAB,
            0,
            &[after_slot3 as u32, (after_slot3 >> 32) as u32],
        ),
    ]);
    assert!(configure_ds_active_pending_slot(
        &mut fsl,
        GdbaScene::TwoPerOnu,
        1,
        0x83,
        true,
        &mut pending_initialized,
    )
    .is_err());
}

#[test]
fn ds_active_pending_sence3_spans_two_rows_and_b_all_preserves_upper_bits() {
    let mut pending_initialized = true;
    // Scene 8 (sence3), ONU 2 => rows 4 and 5; fill all 4 slots in row 4 -> inserts into row 5 slot 0.
    let full_row = (0x101u64) | (0x102u64 << 9) | (0x103u64 << 18) | (0x104u64 << 27);
    let row5_new = 0x109u64;
    let (mut fsl, bus) = mock_fsl(vec![
        read(GDBA_DS_TAB_STA, 0),
        read_mem(
            GDBA_DS_PENDING_TAB,
            4,
            &[full_row as u32, (full_row >> 32) as u32],
        ),
        read_mem(GDBA_DS_PENDING_TAB, 5, &[0, 0]),
        write_mem(
            GDBA_DS_PENDING_TAB,
            4,
            &[full_row as u32, (full_row >> 32) as u32],
        ),
        write_mem(GDBA_DS_PENDING_TAB, 5, &[row5_new as u32, 0]),
        write(GDBA_DS_SOFT_REQ_CFG, &[1]),
        write(GDBA_DS_TAB_CFG, &[0x100]),
        write_mem(
            GDBA_DS_ACTIVE_TAB,
            4,
            &[full_row as u32, (full_row >> 32) as u32],
        ),
        write_mem(GDBA_DS_ACTIVE_TAB, 5, &[row5_new as u32, 0]),
    ]);
    configure_ds_active_pending_slot(
        &mut fsl,
        GdbaScene::EightPerOnu,
        2,
        9,
        true,
        &mut pending_initialized,
    )
    .unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    assert_eq!(compute_b_all_set_thd(1000, 2, 400, false), 900);
    assert_eq!(
        compute_b_all_set_thd(1000, 2, 400, true),
        ((239 * 900) / 0xff) as u16
    );
    assert_eq!(compute_b_all_set_thd(32, 2, 0, false), 0);

    // GDBA_B_ALL_CFG is 58 bits: preserve B_ALL_MODE (bit 57 => w1 bit 25) & ALLOC_IFIFO_NUM_THR (w1 bits 24:16).
    let (mut fsl, bus) = mock_fsl(vec![
        read_reg_words(GDBA_B_ALL_CFG, &[0x1234_0000, 0x02ab_0000]),
        write(GDBA_B_ALL_CFG, &[0x1234_0888, 0x02ab_0888]),
    ]);
    write_b_all_set_thd(&mut fsl, 0x0888).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn dba_calc_cycle_and_granularity_match_stock_table() {
    for (work_cycle, slot_bits, expected_cycle) in [
        (0u32, 1u32, 0u8),
        (0, 3, 1),
        (0, 7, 2),
        (0, 15, 3),
        (0, 31, 4),
        (2, 31, 6),
    ] {
        let (mut fsl, bus) = mock_fsl(vec![
            read(GDBA_WORK_CFG, work_cycle << 1),
            read_mem(GDBA_SLOT_TAB, 0, &[slot_bits << 1]),
        ]);
        assert_eq!(read_dba_calc_cycle(&mut fsl).unwrap(), expected_cycle);
        assert!(bus.0.borrow().steps.is_empty());
    }

    let (mut fsl, bus) = mock_fsl(vec![
        read(GDBA_WORK_CFG, 0),
        read_mem(GDBA_SLOT_TAB, 0, &[31 << 1]),
    ]);
    assert_eq!(read_eight_times_dba_granularity(&mut fsl).unwrap(), 64);
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn recalculate_b_all_bandwidth_skips_when_mode_disabled() {
    let (mut fsl, bus) = mock_fsl(vec![read_reg_words(
        GDBA_B_ALL_CFG,
        &[38880, 38880], // bit 25 (B_ALL_MODE) == 0
    )]);
    let res = recalculate_b_all_bandwidth(&mut fsl, 38880, &GdbaGapConfig::default()).unwrap();
    assert_eq!(res, None);
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn recalculate_b_all_bandwidth_updates_threshold_from_live_gap_and_fec() {
    // guard=4, type3=16, calc_cycle=4 => unit_gap = 8*(4+16+6) = 208; active=2 => total_gap=416.
    // fec active on ONU 1 => value = (38880 - 416/4) * 239 / 255 = 38776 * 239 / 255 = 36343.
    let expected = ((239u32 * (38880 - 104)) / 0xff) as u16;
    let b_all_hi = (1u32 << 25) | (0xab << 16) | 38880;
    let (mut fsl, bus) = mock_fsl(vec![
        read_reg_words(GDBA_B_ALL_CFG, &[38880, b_all_hi]),
        read(GPON_GUARD_TIME_CFG, 0x0004),
        read(GPON_EXTEND_BURST_CFG, 0x1000),
        read(GDBA_WORK_CFG, 0),
        read_mem(GDBA_SLOT_TAB, 0, &[31 << 1]),
        read_reg_words(BMP_CFG_ONU_ACTIVE, &[0b11, 0, 0, 0]),
        read_reg_words(BMP_CFG_ONU_ACTIVE, &[0b11, 0, 0, 0]),
        read_reg_words(BMP_CFG_ONU_FEC_1G, &[0b10, 0, 0, 0]),
        read_reg_words(GDBA_B_ALL_CFG, &[38880, b_all_hi]),
        write(
            GDBA_B_ALL_CFG,
            &[
                u32::from(expected),
                (b_all_hi & !0xffff) | u32::from(expected),
            ],
        ),
    ]);

    let updated = recalculate_b_all_bandwidth(&mut fsl, 38880, &GdbaGapConfig::default()).unwrap();
    assert_eq!(updated, Some(expected));
    assert!(bus.0.borrow().steps.is_empty());
}
