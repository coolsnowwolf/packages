use super::defaults::*;
use super::serdes_profiles::{efuse_0x30_transform, ETH_EFUSE_ADDRESS};
use super::*;
use crate::platform::fiberhome_an4475_g2::{
    activation::{PORTID_CFG_TAB, UP_WMCI_AES_EN_CFG_TAB},
    gdba::GdbaScene,
    polling::PollPolicy,
    protocol::{crc8, Bus, Fsl},
    test_support::{read, write, Mock, Script},
};
use std::{cell::RefCell, collections::HashMap, io, num::NonZeroU16, rc::Rc, time::Duration};

#[derive(Default)]
struct FakeChipState {
    regs: HashMap<u32, Vec<u32>>,
    mems: HashMap<(u32, u32), Vec<u32>>,
    apb_writes: Vec<(u8, u16, u8)>,
    apb_bus_vals: HashMap<(u8, u16), u8>,
    reg_writes: Vec<(u32, Vec<u32>)>,
}

#[derive(Clone)]
struct FakeChip(Rc<RefCell<FakeChipState>>);

impl FakeChip {
    fn new(efuse_raw: u8, serdes_status: u32, ram_status: u32, onu_id_offset: u8) -> Self {
        let mut st = FakeChipState::default();
        st.apb_bus_vals
            .insert((EFUSE_APB_BUS, ETH_EFUSE_ADDRESS), efuse_raw);
        st.regs.insert(SERDES_STATUS, vec![serdes_status]);
        st.regs.insert(RAM_INIT_STATUS, vec![ram_status]);
        st.regs
            .insert(GPON_ONUID_OFFSET_CFG, vec![u32::from(onu_id_offset)]);
        Self(Rc::new(RefCell::new(st)))
    }
}

fn decode_be_words(bytes: &[u8]) -> Vec<u32> {
    let (chunks, rem) = bytes.as_chunks::<4>();
    debug_assert!(rem.is_empty());
    chunks.iter().map(|c| u32::from_be_bytes(*c)).collect()
}

impl Bus for FakeChip {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> io::Result<()> {
        assert!(tx.len() >= 5);
        assert_eq!(crc8(&tx[..4]), tx[4], "SPI header CRC mismatch");
        let op = tx[0] & 0x7;
        let addr = (u32::from(tx[0] >> 3) << 8) | u32::from(tx[1]);
        let count = usize::from(tx[2] >> 4) + 1;
        let row = (u32::from(tx[2] & 0x0f) << 8) | u32::from(tx[3]);
        let mut st = self.0.borrow_mut();

        match op {
            // Register write
            1 => {
                assert_eq!(tx.len(), 5 + count * 4);
                assert!(rx.is_empty());
                let words = decode_be_words(&tx[5..]);
                st.reg_writes.push((addr, words.clone()));
                st.regs.insert(addr, words.clone());

                if addr == 0x1103 {
                    let bus = st.regs.get(&0x1100).map(|w| w[0] as u8).unwrap_or(0);
                    let cmd = words[0];
                    let apb_addr = (cmd & 0xffff) as u16;
                    if cmd & (1 << 16) != 0 {
                        let wdata = st.regs.get(&0x1101).map(|w| w[0] as u8).unwrap_or(0);
                        st.apb_writes.push((bus, apb_addr, wdata));
                        st.apb_bus_vals.insert((bus, apb_addr), wdata);
                    } else {
                        let rdata = st.apb_bus_vals.get(&(bus, apb_addr)).copied().unwrap_or(0);
                        st.regs.insert(0x1102, vec![u32::from(rdata)]);
                    }
                    st.regs.insert(0x1103, vec![cmd | (1 << 17)]);
                }
            }
            // Register read
            2 => {
                assert_eq!(tx.len(), 6);
                assert_eq!(rx.len(), count * 4);
                let default = vec![0; count];
                let words = st.regs.get(&addr).unwrap_or(&default);
                for i in 0..count {
                    let w = words.get(i).copied().unwrap_or(0);
                    rx[i * 4..(i + 1) * 4].copy_from_slice(&w.to_be_bytes());
                }
            }
            // Memory write (reversed words on wire)
            5 => {
                assert_eq!(tx.len(), 5 + count * 4);
                assert!(rx.is_empty());
                let mut words = decode_be_words(&tx[5..]);
                words.reverse();
                st.mems.insert((addr, row), words);
            }
            // Memory read (reversed words on wire)
            6 => {
                assert_eq!(tx.len(), 6);
                assert_eq!(rx.len(), count * 4);
                let default = vec![0; count];
                let words = st.mems.get(&(addr, row)).unwrap_or(&default);
                for i in 0..count {
                    let w = words.get(count - 1 - i).copied().unwrap_or(0);
                    rx[i * 4..(i + 1) * 4].copy_from_slice(&w.to_be_bytes());
                }
            }
            other => panic!("unexpected FSL SPI opcode {other}"),
        }
        Ok(())
    }
}

#[test]
fn efuse_transform_matches_decompiled_formula_for_all_bytes() {
    for raw in 0..=255u8 {
        let b0 = ((raw >> 2) ^ raw ^ (raw >> 7) ^ (raw >> 1)) & 1;
        let b1 = ((raw >> 2) ^ (raw >> 1) ^ (raw >> 3)) & 1;
        let b2 = (raw ^ (raw >> 7) ^ (raw >> 1) ^ (raw >> 4) ^ (raw >> 3)) & 1;
        let b3 = ((raw >> 1) ^ (raw >> 5) ^ (raw >> 4) ^ (raw >> 2)) & 1;
        let b4 = (raw ^ (raw >> 3) ^ (raw >> 7) ^ (raw >> 6) ^ (raw >> 1) ^ (raw >> 5)) & 1;
        let b5 = (raw ^ (raw >> 6) ^ (raw >> 4)) & 1;
        let b6 = ((raw >> 5) ^ (raw >> 7) ^ (raw >> 1)) & 1;
        let b7 = ((raw >> 7) ^ (raw >> 6) ^ (raw >> 1) ^ raw) & 1;
        let expected = (b0
            | (b1 << 1)
            | (b2 << 2)
            | (b3 << 3)
            | (b4 << 4)
            | (b5 << 5)
            | (b6 << 6)
            | (b7 << 7))
            ^ 0xe1;
        assert_eq!(
            efuse_0x30_transform(raw),
            expected,
            "mismatch for raw={raw:#x}"
        );
    }
}

#[test]
fn wait_ram_initialized_polls_0x31_for_0xbf_mask() {
    let script = Script {
        steps: vec![read(RAM_INIT_STATUS, 0x15), read(RAM_INIT_STATUS, 0xfff)].into(),
        calls: 0,
        fail_at: 0,
    };
    let bus = Mock(Rc::new(RefCell::new(script)));
    let mut fsl = Fsl::new(bus.clone());
    let mut slept = Vec::new();
    let val = wait_ram_initialized(
        &mut fsl,
        PollPolicy {
            max_reads: NonZeroU16::new(4).unwrap(),
            interval: Duration::from_millis(1),
        },
        &mut |d| {
            slept.push(d);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(val, 0xfff);
    assert_eq!(slept, vec![Duration::ZERO, Duration::from_millis(1)]);
}

#[test]
fn release_reset_default_and_alternate_profiles_program_expected_registers_and_apb() {
    let chip_def = FakeChip::new(0x5a, SERDES_LOCK_MASK, GPON_RAM_INIT_MASK, 4);
    let mut fsl_def = Fsl::new(chip_def.clone());
    let mut slept_def = Duration::ZERO;
    release_reset(&mut fsl_def, &InitConfig::default(), &mut |d| {
        slept_def += d;
        Ok(())
    })
    .unwrap();

    let st_def = chip_def.0.borrow();
    // 502 PON + 502 ETH + 1 efuse writeback = 1005 APB writes
    assert_eq!(st_def.apb_writes.len(), 1005);
    assert_eq!(st_def.apb_writes[261], (1, 0x1003, 0xe9));
    assert_eq!(st_def.apb_writes[502 + 261], (0, 0x1003, 0xed));
    assert_eq!(st_def.apb_writes[502 + 14], (0, 0x040e, 44));
    assert_eq!(st_def.apb_writes[502 + 328], (0, 0x2006, 23));
    assert_eq!(
        st_def.apb_writes[1004],
        (2, ETH_EFUSE_ADDRESS, efuse_0x30_transform(0x5a))
    );
    assert_eq!(st_def.regs.get(&HSST_CFG).unwrap(), &vec![1]);
    assert_eq!(st_def.regs.get(&PCS_INIT).unwrap(), &vec![0xff]);
    assert_eq!(st_def.regs.get(&ETH_CKG_CFG).unwrap(), &vec![3]);
    assert_eq!(
        st_def.regs.get(&SHM_CFG_MATRIX_INIT).unwrap(),
        &vec![u32::MAX, 0xffff]
    );
    assert!(slept_def >= Duration::from_millis(226));

    // Alternate clock + 1.25G ETH rate + <=64 T-CONTs MAU matrix (16 ONUs * 4 T-CONTs)
    let chip_alt = FakeChip::new(0xa3, SERDES_LOCK_MASK, GPON_RAM_INIT_MASK, 0);
    let mut fsl_alt = Fsl::new(chip_alt.clone());
    let alt_cfg = InitConfig {
        clock_source: ClockSourceMode::Alternate,
        eth_rate: EthSerdesRate::Rate1p25G,
        max_onu_id: 16,
        gdba_scene: GdbaScene::FourPerOnu,
        ..InitConfig::default()
    };
    release_reset(&mut fsl_alt, &alt_cfg, &mut |_| Ok(())).unwrap();
    let st_alt = chip_alt.0.borrow();
    assert_eq!(st_alt.apb_writes[261], (1, 0x1003, 0xc9));
    assert_eq!(st_alt.apb_writes[502 + 261], (0, 0x1003, 0xc9));
    assert_eq!(st_alt.apb_writes[502 + 14], (0, 0x040e, 48));
    assert_eq!(st_alt.apb_writes[502 + 328], (0, 0x2006, 3));
    assert!(!st_alt.regs.contains_key(&HSST_CFG));
    assert_eq!(
        st_alt.regs.get(&SHM_CFG_MATRIX_INIT).unwrap(),
        &vec![0x0f00_ffff, 0x00f0]
    );
}

#[test]
fn serdes_logic_reset_times_out_cleanly_without_releasing_pcs() {
    let chip = FakeChip::new(0, 0, GPON_RAM_INIT_MASK, 0);
    let mut fsl = Fsl::new(chip.clone());
    let err = reset_and_release_serdes_logic(
        &mut fsl,
        ClockSourceMode::Default,
        PollPolicy {
            max_reads: NonZeroU16::new(3).unwrap(),
            interval: Duration::from_millis(1),
        },
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(!chip.0.borrow().regs.contains_key(&PCS_INIT));
}

#[test]
fn apply_gpon_defaults_preserves_neighbor_bits_and_configures_gems() {
    let chip = FakeChip::new(0, SERDES_LOCK_MASK, GPON_RAM_INIT_MASK, 12);
    {
        let mut st = chip.0.borrow_mut();
        // Pre-populate multi-word registers with unrelated neighbor bits to verify RMW preservation.
        st.regs
            .insert(GPON_OVERHEAD_CFG, vec![0xdead_be00, 0x1122_3344, 0x5566]);
        st.regs
            .insert(GDBA_L_H_THR_CFG, vec![0x7fff_8000, 0x1234_5678, 0, 1]);
        st.regs
            .insert(BMP_CFG_BOUNDS, vec![0xf000_3fff, 0x8899_aabb]);
    }
    let mut fsl = Fsl::new(chip.clone());
    let offset = apply_gpon_defaults(&mut fsl, 2).unwrap();
    assert_eq!(offset, 12);

    let st = chip.0.borrow();
    assert_eq!(
        st.regs.get(&GPON_OVERHEAD_CFG).unwrap(),
        &vec![0xdead_be20, 0x1122_3344, 0x5566]
    );
    assert_eq!(
        st.regs.get(&GDBA_L_H_THR_CFG).unwrap(),
        &vec![0x7fff_8014, 0x1234_5678, 0, 1]
    );
    assert_eq!(
        st.regs.get(&BMP_CFG_BOUNDS).unwrap(),
        &vec![(0xf000_3fff & !(0x3fff << 14)) | (9719 << 14), 0x8899_aabb]
    );
    assert_eq!(st.regs.get(&BMP_OMCI_ALLOC_VALUE).unwrap(), &vec![1]);
    assert_eq!(st.regs.get(&NNI_INT_MASK).unwrap(), &vec![0xff]);
    assert_eq!(st.regs.get(&BMP_CFG_PLOAMU_CTRL).unwrap(), &vec![0; 8]);
    assert_eq!(st.mems.get(&(GDBA_SLOT_TAB, 0)).unwrap(), &vec![0x3f]);
    for row in 0..4u32 {
        assert_eq!(
            st.mems.get(&(GDBA_SLC_PARA_TAB, row)).unwrap(),
            &vec![u32::MAX]
        );
    }
    // Check ONU 0 GEMs (0 for OMCI, 256..272 packed 4-per-row, WMCI bit on 270)
    assert_eq!(
        st.mems.get(&(PORTID_CFG_TAB, 0)).unwrap()[0] & 0xff,
        0x00, // onu=0, flag=0
    );
    // ONU 1 OMCI GEM = 1 -> byte 1 of row 0 in PORTID_CFG_TAB: 0x01 << 8
    // Wait: both ONU 0 GEM 0 (0x00) and ONU 1 GEM 1 (0x01) are enabled in row 0; neighboring bytes untouched.
    // Wait note ONU 0 GEM 0 encoded value is also 0x00; ONU 1 GEM 1 is 0x01 in bits 8..15.
    assert_eq!(
        (st.mems.get(&(PORTID_CFG_TAB, 0)).unwrap()[0] >> 8) & 0xff,
        0x01
    );
    // ONU 1 GEM 272..287 -> row 68..71, slot byte = (1 | 0) = 0x01 in every byte
    assert_eq!(
        st.mems.get(&(PORTID_CFG_TAB, 272 >> 2)).unwrap()[0],
        0x0101_0101
    );
    // WMCI enabled on slot 14 for ONU 0 (gem 270 -> row 33 bit 6+8) and ONU 1 (gem 286 -> row 35 bit 6+8)
    assert_eq!(
        st.mems.get(&(UP_WMCI_AES_EN_CFG_TAB, 270 >> 3)).unwrap()[0],
        1 << (6 + 8)
    );
    assert_eq!(
        st.mems.get(&(UP_WMCI_AES_EN_CFG_TAB, 286 >> 3)).unwrap()[0],
        1 << (6 + 8)
    );
}

#[test]
fn release_reset_pre_writes_expected_sequence() {
    let steps = vec![
        write(GLOBAL_RST_FUCI, &[u32::MAX]),
        write(SPI_MISO_CFG, &[SPI_MISO_DEFAULT]),
        write(GLOBAL_RST_LOGIC, &[1]),
    ];
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: steps.into(),
        calls: 0,
        fail_at: 0,
    })));
    let mut fsl = Fsl::new(bus.clone());
    release_reset_pre(&mut fsl).unwrap();
    assert_eq!(bus.0.borrow().calls, 3);
}

#[test]
fn configure_nni_vport_programs_gem_offset_and_tpid_preserving_upper_bits() {
    let chip = FakeChip::new(0, SERDES_LOCK_MASK, GPON_RAM_INIT_MASK, 12);
    {
        let mut st = chip.0.borrow_mut();
        st.regs.insert(NNI_GEMPORT_OFFSET_CFG, vec![0xabcd_f000]);
        st.regs.insert(NNI_ETH_TPID_CFG, vec![0x000d_0000]);
    }
    let mut fsl = Fsl::new(chip.clone());
    configure_nni_vport(&mut fsl, DEFAULT_NNI_GEMPORT_OFFSET, DEFAULT_NNI_ETH_TPID).unwrap();

    let st = chip.0.borrow();
    assert_eq!(
        st.regs.get(&NNI_GEMPORT_OFFSET_CFG).unwrap(),
        &vec![0xabcd_f0ff]
    );
    assert_eq!(st.regs.get(&NNI_ETH_TPID_CFG).unwrap(), &vec![0x000d_8200]);
    assert_eq!(st.regs.get(&VLAN_TYPE_CFG).unwrap(), &vec![0x8200]);

    let err = set_nni_gem_port_offset(&mut fsl, 0x1000).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
