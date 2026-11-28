use super::*;
use crate::platform::fiberhome_an4475_g2::test_support::{read, write, Mock, Script, Step};
use std::{cell::RefCell, rc::Rc};

fn mock_fsl(steps: Vec<Step>) -> (Fsl<Mock>, Mock) {
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: steps.into(),
        ..Script::default()
    })));
    (Fsl::new(bus.clone()), bus)
}

#[test]
fn read_status_decodes_all_sources_and_clamps_all_ones() {
    let (mut fsl, bus) = mock_fsl(vec![
        read(GPON_INT, 0b010_1111),
        read(PLOAM_LOAMI_SUFI_INT, 0b11),
        read(PLOAM_RX_INT, 0),
        read(PLOAM_RD_VLD_CFG, 1),
        read(SOFT_RDWR_OMCI_STA, 1),
        read(GPON_INT, u32::MAX),
        read(PLOAM_LOAMI_SUFI_INT, 0),
        read(PLOAM_RX_INT, u32::MAX),
        read(PLOAM_RD_VLD_CFG, 0),
        read(SOFT_RDWR_OMCI_STA, 0),
    ]);

    let flags = read_status(&mut fsl).expect("read_status");
    assert_eq!(
        flags,
        GponInterruptFlags {
            tiwi: true,
            dowi: true,
            lofi: true,
            losi: true,
            rssi: false,
            dying_gasp: true,
            sufi: true,
            loami: true,
            ploam_rx: true,
            omci_rx: true,
        }
    );

    let clamped = read_status(&mut fsl).expect("clamped read_status");
    assert_eq!(clamped, GponInterruptFlags::default());
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn set_mask_and_read_mask_roundtrip_and_preserve_neighbors() {
    let (mut fsl, bus) = mock_fsl(vec![
        read(GPON_INT_MASK, 1 << 6), // LOF_INT_MASK preserved
        write(GPON_INT_MASK, &[(1 << 6) | BIT_LOSI]),
        read(PLOAM_LOAMI_SUFI_INT_MASK, 0),
        write(PLOAM_LOAMI_SUFI_INT_MASK, &[BIT_SUFI]),
        read(PLOAM_RX_INT_MASK, 0),
        write(PLOAM_RX_INT_MASK, &[BIT_PLOAM_RD]),
        read(NNI_INT_MASK, 0),
        write(NNI_INT_MASK, &[BIT_OMCI_RX]),
        read(GPON_INT_MASK, (1 << 6) | BIT_LOSI),
        read(PLOAM_LOAMI_SUFI_INT_MASK, BIT_SUFI),
        read(PLOAM_RX_INT_MASK, BIT_PLOAM_RD),
        read(PLOAM_RD_VLD_CFG, 0),
        read(NNI_INT_MASK, BIT_OMCI_RX),
        write(GPON_INT_MASK, &[u32::MAX]),
        write(GPON_INT_MASK, &[0]),
    ]);

    set_mask(&mut fsl, GponInterrupt::Losi, true).expect("enable losi");
    set_mask(&mut fsl, GponInterrupt::SuF, true).expect("enable suf");
    set_mask(&mut fsl, GponInterrupt::PloamRx, true).expect("enable ploam");
    set_mask(&mut fsl, GponInterrupt::OmciRx, true).expect("enable omci");

    let mask = read_mask(&mut fsl).expect("read_mask");
    assert!(mask.losi && mask.sufi && mask.ploam_rx && mask.omci_rx);
    assert!(!mask.loami && !mask.dying_gasp);

    set_all_gpon_int_masks(&mut fsl, true).expect("enable all gpon masks");
    set_all_gpon_int_masks(&mut fsl, false).expect("disable all gpon masks");
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn poll_and_clear_pending_respects_masks_and_stock_order() {
    let (mut fsl, bus) = mock_fsl(vec![
        // read_status: TiWi, LoSi, RSSI, SuF, LoAmi, PloamRx, OmciRx
        read(GPON_INT, BIT_TIWI | BIT_LOSI | BIT_RSSI),
        read(PLOAM_LOAMI_SUFI_INT, BIT_SUFI | BIT_LOAMI),
        read(PLOAM_RX_INT, BIT_PLOAM_RD),
        read(PLOAM_RD_VLD_CFG, 0),
        read(SOFT_RDWR_OMCI_STA, BIT_OMCI_RX),
        // read_mask: LoAmi masked; RSSI unmasked in HW reg but ignored by DISPATCH_ORDER
        read(GPON_INT_MASK, BIT_TIWI | BIT_LOSI | BIT_RSSI),
        read(PLOAM_LOAMI_SUFI_INT_MASK, BIT_SUFI),
        read(PLOAM_RX_INT_MASK, BIT_PLOAM_RD),
        read(PLOAM_RD_VLD_CFG, 0),
        read(NNI_INT_MASK, BIT_OMCI_RX),
        // W1C clears in exact DISPATCH_ORDER
        write(PLOAM_RX_INT, &[BIT_PLOAM_RD]),
        write(GPON_INT, &[BIT_LOSI]),
        write(GPON_INT, &[BIT_TIWI]),
        write(PLOAM_LOAMI_SUFI_INT, &[BIT_SUFI]),
        write(NNI_INT, &[BIT_OMCI_RX]),
    ]);

    let dispatched = poll_and_clear_pending(&mut fsl).expect("poll_and_clear_pending");
    assert_eq!(
        dispatched,
        vec![
            GponInterrupt::PloamRx,
            GponInterrupt::Losi,
            GponInterrupt::Tiwi,
            GponInterrupt::SuF,
            GponInterrupt::OmciRx,
        ]
    );
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn alarm_bitmap_reads_four_words_and_checks_onu_index() {
    let expected: [u32; 4] = [1 << 3, 1 << 0, 0, 1 << 31];
    let rx: Vec<u8> = expected.iter().flat_map(|w| w.to_be_bytes()).collect();
    let step = crate::platform::fiberhome_an4475_g2::test_support::message(
        [
            ((GPON_LOSI_ALM >> 8) as u8) << 3 | 2,
            GPON_LOSI_ALM as u8,
            3 << 4,
            0,
            0,
        ],
        &[0],
        rx,
    );
    let (mut fsl, bus) = mock_fsl(vec![step]);
    let words = read_alarm_bitmap(&mut fsl, GPON_LOSI_ALM).expect("read_alarm_bitmap");
    assert!(alarm_bitmap_contains(&words, 3));
    assert!(!alarm_bitmap_contains(&words, 2));
    assert!(alarm_bitmap_contains(&words, 32));
    assert!(alarm_bitmap_contains(&words, 127));
    assert!(!alarm_bitmap_contains(&words, 128));
    assert!(read_alarm_bitmap(&mut fsl, GPON_INT).is_err());
    assert!(bus.0.borrow().steps.is_empty());
}
#[test]
fn idle_interrupt_poll_issues_regular_reads_without_heap_allocations() {
    struct IdleBus(usize);
    impl super::Bus for IdleBus {
        fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> std::io::Result<()> {
            assert_eq!(tx.len(), 6);
            assert_eq!(rx.len(), 4);
            rx.fill(0);
            self.0 += 1;
            Ok(())
        }
    }
    let mut fsl = super::Fsl { bus: IdleBus(0) };
    let allocations = crate::memory::allocation_test::allocations(|| {
        for _ in 0..256 {
            assert!(super::poll_and_clear_pending(&mut fsl).unwrap().is_empty());
        }
    });
    assert_eq!(fsl.bus.0, 256 * 10);
    assert_eq!(allocations, 0);
}
