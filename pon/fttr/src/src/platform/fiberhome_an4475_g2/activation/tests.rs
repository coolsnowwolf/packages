use super::*;
use crate::platform::fiberhome_an4475_g2::{
    test_support::{cancel_at, message, policy, proceed, read, write, Mock, Script, Step},
    upstream::PloamRecord,
};
use std::{cell::RefCell, rc::Rc};

fn mock_fsl(steps: Vec<Step>) -> (Fsl<Mock>, Mock) {
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: steps.into(),
        ..Script::default()
    })));
    (Fsl::new(bus.clone()), bus)
}

fn read_words(address: u32, words: &[u32]) -> Step {
    message(
        [
            ((address >> 8) as u8) << 3 | 2,
            address as u8,
            ((words.len() - 1) as u8) << 4,
            0,
            0,
        ],
        &[0],
        words.iter().flat_map(|v| v.to_be_bytes()).collect(),
    )
}

fn read_mem_word(address: u32, row: u32, word: u32) -> Step {
    message(
        [
            ((address >> 8) as u8) << 3 | 6,
            address as u8,
            (row >> 8) as u8,
            row as u8,
            0,
        ],
        &[0],
        word.to_be_bytes().to_vec(),
    )
}

fn write_mem_word(address: u32, row: u32, word: u32) -> Step {
    message(
        [
            ((address >> 8) as u8) << 3 | 5,
            address as u8,
            (row >> 8) as u8,
            row as u8,
            0,
        ],
        &word.to_be_bytes(),
        vec![],
    )
}

#[test]
fn ploam_record_extracts_ranging_and_identity_fields() {
    let mut words = [0u32; 13];
    words[0] = (0x123 << 5) | (1 << 4);
    words[4] = 0x05;
    words[3] = 0x0100_0000;
    let rec = PloamRecord::decode(words, true);
    assert!(rec.stock_error_flag());
    assert_eq!(rec.ranging_time_raw(), 0x123 + 0x30);
    assert_eq!(rec.onu_id(), Some(5));
    assert_eq!(rec.message_id(), 1);
    assert_eq!(rec.rate_metadata(), 0);

    words[4] = 0xff;
    let unassigned = PloamRecord::decode(words, false);
    assert_eq!(unassigned.onu_id(), None);
}

#[test]
fn eqd_and_downstream_ploam_frames_match_stock_layout() {
    assert_eq!(eqd_from_ranging(0x100, true), 0x80 + 0x18);
    assert_eq!(eqd_from_ranging(0x100, false), 0x100);
    assert_eq!(eqd_from_ranging(0x20_002f, false), 0x20_002f);
    assert_eq!(eqd_from_ranging(0x20_002f, true), 0x10_002f);

    let ranging = ranging_time_ploam(7, 0x1234_5678);
    assert_eq!(
        ranging,
        [7, 0x04, 0x00, 0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0, 0]
    );

    let cfg = configure_port_id_ploam(7, 0x1ab, true, 0x0203).unwrap();
    assert_eq!(cfg, [7, 0x0e, 0x01, 0x1a, 0xb0, 0x02, 0x03, 0, 0, 0, 0, 0]);
    assert!(configure_port_id_ploam(7, 0x1000, true, 1).is_err());
}

#[test]
fn onu_index_validates_offset_and_7bit_bound() {
    assert_eq!(onu_index(0, 0).unwrap(), 0);
    assert_eq!(onu_index(127, 0).unwrap(), 127);
    assert_eq!(onu_index(130, 10).unwrap(), 120);
    assert!(onu_index(5, 10).is_err());
    assert!(onu_index(128, 0).is_err());
}

#[test]
fn active_bitmap_rmw_preserves_neighboring_bits() {
    let (mut fsl, bus) = mock_fsl(vec![
        read_words(
            BMP_CFG_ONU_ACTIVE,
            &[0xaaaa_0000, 0x0000_0001, 0, 0x8000_0000],
        ),
        write(
            BMP_CFG_ONU_ACTIVE,
            &[0xaaaa_0000, 0x0000_0005, 0, 0x8000_0000],
        ),
        read_words(ALM_PROC_ONU_ACTIVE_CFG, &[0, 0, 0, 0xffff_ffff]),
        write(ALM_PROC_ONU_ACTIVE_CFG, &[0, 0, 0, 0x7fff_ffff]),
    ]);
    set_onu_active_bit(&mut fsl, BMP_CFG_ONU_ACTIVE, 34, true).unwrap();
    set_onu_active_bit(&mut fsl, ALM_PROC_ONU_ACTIVE_CFG, 127, false).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    assert!(set_onu_active_bit(&mut fsl, 0x999, 0, true).is_err());
    assert!(set_onu_active_bit(&mut fsl, BMP_CFG_ONU_ACTIVE, 128, true).is_err());
}

#[test]
fn gem_port_table_rmw_preserves_neighboring_slots() {
    // GEM 0x101 -> row 0x40, slot 1 (shift 8); flag=1, onu_index=3 -> 0x83 << 8.
    let (mut fsl, bus) = mock_fsl(vec![
        read_mem_word(PORTID_CFG_TAB, 0x40, 0xaa55_0011),
        write_mem_word(PORTID_CFG_TAB, 0x40, 0xaa55_8311),
        read_mem_word(PORTID_CFG_TAB, 0x40, 0xaa55_8311),
        write_mem_word(PORTID_CFG_TAB, 0x40, 0xaa55_0011),
    ]);
    configure_gem_port(&mut fsl, 3, 1, 0x101, true).unwrap();
    configure_gem_port(&mut fsl, 3, 1, 0x101, false).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    assert!(configure_gem_port(&mut fsl, 128, 0, 0x101, true).is_err());
    assert!(configure_gem_port(&mut fsl, 3, 2, 0x101, true).is_err());
    assert!(configure_gem_port(&mut fsl, 3, 0, 0x1000, true).is_err());
}

#[test]
fn range_request_writes_req_polls_ack_and_pulses_pls_req() {
    let (mut fsl, bus) = mock_fsl(vec![
        write(BMP_CFG_RG_REQ, &[0, (1 << 7) | 5]),
        read(BMP_STA, 0),
        read(BMP_STA, BMP_STA_RANGE_ACK),
        write(BMP_PLS_REQ, &[BMP_PLS_RG_REQ]),
    ]);
    range_request(&mut fsl, 5, policy(3), &mut proceed).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    assert!(range_request(&mut fsl, 128, policy(1), &mut proceed).is_err());

    let (mut fsl, _) = mock_fsl(vec![
        write(BMP_CFG_RG_REQ, &[0, (1 << 7) | 1]),
        read(BMP_STA, 0),
    ]);
    assert_eq!(
        range_request(&mut fsl, 1, policy(1), &mut proceed)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );

    let (mut fsl, bus) = mock_fsl(vec![]);
    let mut cancel = cancel_at(1);
    assert_eq!(
        range_request(&mut fsl, 1, policy(2), &mut cancel)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(bus.0.borrow().calls, 0);
}

#[test]
fn wmci_aes_table_rmw_preserves_other_gem_slots() {
    // GEM 0x10e -> row 0x21, bit 6 (AES bit 6, WMCI bit 14).
    let (mut fsl, bus) = mock_fsl(vec![
        read_mem_word(UP_WMCI_AES_EN_CFG_TAB, 0x21, 0x0101),
        write_mem_word(UP_WMCI_AES_EN_CFG_TAB, 0x21, 0x4101),
        read_mem_word(UP_WMCI_AES_EN_CFG_TAB, 0x21, 0x4101),
        write_mem_word(UP_WMCI_AES_EN_CFG_TAB, 0x21, 0x0141),
    ]);
    set_gem_wmci_aes(&mut fsl, 0x10e, true, false).unwrap();
    set_gem_wmci_aes(&mut fsl, 0x10e, false, true).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(set_gem_wmci_aes(&mut fsl, 0x1000, true, false).is_err());
}

#[test]
fn gem_statis_enable_writes_9bit_entries_and_checks_bounds() {
    let (mut fsl, bus) = mock_fsl(vec![
        write_mem_word(STATIS_US_GEM_EN_TAB, 0x101, 0x105),
        write_mem_word(STATIS_DS_GEM_EN_TAB, 0x101, 0x005),
    ]);
    set_gem_statis_enable(&mut fsl, GemStatisDirection::Upstream, 0x101, true, 5).unwrap();
    set_gem_statis_enable(&mut fsl, GemStatisDirection::Downstream, 0x101, false, 5).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(
        set_gem_statis_enable(&mut fsl, GemStatisDirection::Upstream, 0x1000, true, 0).is_err()
    );
}

#[test]
fn switch_gemport_bitmap_rmw_preserves_neighboring_63_bits() {
    // GEM 0x10e = 270 -> bank 4 (addr 0xb09), word 0 bit 14.
    // GEM 0x120 = 288 -> bank 4 (addr 0xb09), word 1 bit 0.
    let (mut fsl, bus) = mock_fsl(vec![
        read_words(0xb09, &[0x0000_0001, 0x8000_0000]),
        write(0xb09, &[0x0000_4001, 0x8000_0000]),
        read_words(0xb09, &[0x0000_4001, 0x8000_0001]),
        write(0xb09, &[0x0000_4001, 0x8000_0000]),
    ]);
    set_switch_gemport_bitmap(&mut fsl, 0x10e, true).unwrap();
    set_switch_gemport_bitmap(&mut fsl, 0x120, false).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(set_switch_gemport_bitmap(&mut fsl, 0x1000, true).is_err());
}

#[test]
fn assign_and_deactivate_ploam_frames_match_stock_layout() {
    let sn = *b"FHLK0001";
    let assign = assign_onu_id_ploam(17, &sn);
    assert_eq!(
        assign,
        [0xff, 0x03, 17, b'F', b'H', b'L', b'K', b'0', b'0', b'0', b'1', 0]
    );
    assert_eq!(
        deactivate_onu_ploam(17),
        [17, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn us_fec_and_upstream_rate_bitmaps_preserve_neighbor_onus() {
    let (mut fsl, bus) = mock_fsl(vec![
        read_words(BMP_CFG_ONU_FEC_1G, &[0x0000_0001, 0, 0, 0x8000_0000]),
        write(BMP_CFG_ONU_FEC_1G, &[0x0000_0003, 0, 0, 0x8000_0000]),
        read_words(BMP_CFG_ONU_FEC_1G, &[0x0000_0003, 0, 0, 0x8000_0000]),
        read_words(GPON_US_RATE_SEL_CFG, &[0, 0x10, 0, 0]),
        write(GPON_US_RATE_SEL_CFG, &[0, 0x12, 0, 0]),
        read_words(BMP_CFG_UP_RATE_IND, &[0, 0x10, 0, 0]),
        write(BMP_CFG_UP_RATE_IND, &[0, 0x12, 0, 0]),
    ]);
    set_onu_us_fec(&mut fsl, 1, true).unwrap();
    assert!(get_onu_us_fec(&mut fsl, 1).unwrap());
    set_onu_upstream_rate_2p5g(&mut fsl, 33, true).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(set_onu_us_fec(&mut fsl, 128, true).is_err());
    assert!(get_onu_us_fec(&mut fsl, 128).is_err());
}

#[test]
fn force_ploamu_and_filter_slots_program_expected_bits() {
    let (mut fsl, bus) = mock_fsl(vec![
        write(BMP_CFG_PLOAMU_CTRL, &[u32::MAX; 8]),
        write(BMP_CFG_PLOAMU_CTRL, &[0; 8]),
        read(PLOAM_US_FILTER_CFG + 2, 0x0000_8500),
        write(PLOAM_US_FILTER_CFG + 2, &[(1 << 16) | 0x0000_8509]),
        read(PLOAM_US_FILTER_CFG + 2, (1 << 16) | 0x0000_8509),
        write(
            PLOAM_US_FILTER_CFG + 2,
            &[(1 << 16) | (1 << 15) | (42 << 8) | 0x09],
        ),
    ]);
    set_force_ploamu(&mut fsl, true).unwrap();
    set_force_ploamu(&mut fsl, false).unwrap();
    set_us_ploam_filter_msg_id(&mut fsl, 2, 9, true).unwrap();
    set_us_ploam_filter_onu_id(&mut fsl, 2, 42, true).unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(set_us_ploam_filter_msg_id(&mut fsl, 4, 0, true).is_err());
    assert!(set_us_ploam_filter_onu_id(&mut fsl, 0, 128, true).is_err());
}

// The Alloc-ID → T-CONT mapping tests live with the single implementation in
// gdba/tests.rs (`alloc_to_tcont_handles_omcc_and_data_bands`).

#[test]
fn clear_rsm_tcont_polls_completion_and_detects_alarm() {
    let (mut fsl, bus) = mock_fsl(vec![
        write(RSM_CFG_TCONT_CLR, &[(5 << 1) | 1]),
        read(RSM_CFG_TCONT_CLR, (5 << 1) | 1),
        read(RSM_CFG_TCONT_CLR, 5 << 1),
        read(RSM_ALM, 0),
    ]);
    clear_rsm_tcont(&mut fsl, 5, policy(3), &mut proceed).unwrap();
    assert!(bus.0.borrow().steps.is_empty());

    let (mut fsl_alm, _) = mock_fsl(vec![
        write(RSM_CFG_TCONT_CLR, &[(5 << 1) | 1]),
        read(RSM_CFG_TCONT_CLR, 0),
        read(RSM_ALM, 1 << 22),
    ]);
    let err = clear_rsm_tcont(&mut fsl_alm, 5, policy(1), &mut proceed).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
}

#[test]
fn remove_onu_clears_active_fec_deactivates_and_flushes_tconts_and_gems() {
    use crate::platform::fiberhome_an4475_g2::{
        downstream::{pack_ploam, PLOAM_DATA, PLOAM_INFO},
        management::Management,
    };

    let deact_words = pack_ploam(&deactivate_onu_ploam(5));
    let (fsl, bus) = mock_fsl(vec![
        // BMP_CFG_ONU_ACTIVE clear bit 5
        read_words(BMP_CFG_ONU_ACTIVE, &[0x21, 0, 0, 0]),
        write(BMP_CFG_ONU_ACTIVE, &[0x01, 0, 0, 0]),
        // BMP_CFG_ONU_FEC_1G clear bit 5
        read_words(BMP_CFG_ONU_FEC_1G, &[0x20, 0, 0, 0]),
        write(BMP_CFG_ONU_FEC_1G, &[0, 0, 0, 0]),
        // One Deactivate_ONU-ID send
        read(PLOAM_INFO, 0),
        write(PLOAM_DATA, &deact_words),
        write(PLOAM_INFO, &[0x101]),
        read(PLOAM_INFO, 0),
        // Clear T-CONT 5
        write(RSM_CFG_TCONT_CLR, &[(5 << 1) | 1]),
        read(RSM_CFG_TCONT_CLR, 0),
        read(RSM_ALM, 0),
        // Disable GEM 270 (row 67, byte 2)
        read_mem_word(PORTID_CFG_TAB, 270 >> 2, 0x0005_0000),
        write_mem_word(PORTID_CFG_TAB, 270 >> 2, 0),
        // ALM_PROC_ONU_ACTIVE_CFG clear bit 5
        read_words(ALM_PROC_ONU_ACTIVE_CFG, &[0x21, 0, 0, 0]),
        write(ALM_PROC_ONU_ACTIVE_CFG, &[0x01, 0, 0, 0]),
    ]);
    let mut mgmt = Management::new(fsl);
    remove_onu(
        &mut mgmt,
        5,
        5,
        &[5],
        &[270],
        1,
        policy(2),
        policy(2),
        &mut proceed,
    )
    .unwrap();
    assert!(bus.0.borrow().steps.is_empty());
}
