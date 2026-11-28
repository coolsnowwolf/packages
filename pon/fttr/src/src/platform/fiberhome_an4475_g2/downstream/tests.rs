use super::*;
use crate::platform::fiberhome_an4475_g2::{
    initialization::{wait_ram_initialized, GPON_RAM_INIT_MASK, RAM_INIT_STATUS},
    protocol::Fsl,
    test_support::*,
};

fn ploam_steps() -> Vec<Step> {
    vec![
        read(0x40f, 0),
        write(
            0x403,
            &[
                0x0a0b0c00, 0x06070809, 0x02030405, 1, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        ),
        write(0x40f, &[0x101]),
        read(0x40f, 0x101),
        read(0x40f, 1),
    ]
}

const PLOAM: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
const OMCI: [u8; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

fn omci_steps() -> Vec<Step> {
    vec![
        read(0x924, 1), // Upstream status must not block downstream TX.
        read(0x913, 0x12a),
        write(0x913, &[0x1a9]), // Preserve neighboring fields, clear bit1.
        read(0x913, 0x1a9),
        memory(0, &[1, 2, 3, 4, 5, 6, 7, 8]),
        memory(1, &[9, 10, 11, 0, 0, 0, 0, 0]),
        write(0x916, &[0x08017578]), // selection1, 11 bytes, GEM 0xabc.
        write(0x916, &[0x08017579]),
        read(0x924, 3),
        read(0x924, 1),
    ]
}

fn assert_latched(tx: &mut Management<Mock>, bus: &Mock) {
    assert!(tx.needs_reconciliation());
    let calls = bus.0.borrow().calls;
    let error = tx.send_ploam(&PLOAM, policy(2), &mut proceed).unwrap_err();
    assert_eq!(error.submission, Submission::Indeterminate);
    let error = tx
        .send_omci_frame(0xabc, true, &OMCI, policy(2), &mut proceed)
        .unwrap_err();
    assert_eq!(error.submission, Submission::Indeterminate);
    assert_eq!(bus.0.borrow().calls, calls, "latched session performed I/O");
}

#[test]
fn ploam_stock_layout_single_submit_and_busy_handshake() {
    let (mut tx, bus) = session(ploam_steps(), 0);
    let mut delays = Vec::new();
    tx.send_ploam(&PLOAM, policy(2), &mut |delay| {
        delays.push(delay);
        Ok(())
    })
    .unwrap();
    assert!(!tx.needs_reconciliation());
    assert!(bus.0.borrow().steps.is_empty());
    assert_eq!(
        delays,
        [Duration::ZERO; 4]
            .into_iter()
            .chain([Duration::from_micros(150)])
            .collect::<Vec<_>>()
    );
    // Successful completion allows an intentional subsequent enqueue.
    bus.0.borrow_mut().steps.extend(ploam_steps());
    tx.send_ploam(&PLOAM, policy(2), &mut proceed).unwrap();
    assert_eq!(bus.0.borrow().calls, 10);
}

#[test]
fn busy_preflight_never_overwrites_either_buffer() {
    for omci in [false, true] {
        let step = if omci {
            read(0x924, 2)
        } else {
            read(0x40f, 0x100)
        };
        let (mut tx, bus) = session(vec![step], 0);
        let result = if omci {
            tx.send_omci_frame(0xabc, true, &OMCI, policy(2), &mut proceed)
        } else {
            tx.send_ploam(&PLOAM, policy(2), &mut proceed)
        };
        let error = result.unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.submission, Submission::NotSubmitted);
        assert!(!tx.needs_reconciliation());
        assert_eq!(bus.0.borrow().calls, 1);
    }
}

#[test]
fn omci_rows_configuration_and_control_match_stock_wire_order() {
    let (mut tx, bus) = session(omci_steps(), 0);
    tx.send_omci_frame(0xabc, true, &OMCI, policy(2), &mut proceed)
        .unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert!(!tx.needs_reconciliation());
}

#[test]
fn every_spi_failure_stops_immediately_and_submit_uncertainty_is_latched() {
    for omci in [false, true] {
        let count = if omci { 10 } else { 5 };
        let submit = if omci { 8 } else { 3 };
        for fail_at in 1..=count {
            let (mut tx, bus) = session(if omci { omci_steps() } else { ploam_steps() }, fail_at);
            let error = if omci {
                tx.send_omci_frame(0xabc, true, &OMCI, policy(2), &mut proceed)
            } else {
                tx.send_ploam(&PLOAM, policy(2), &mut proceed)
            }
            .unwrap_err();
            assert_eq!(error.source.kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(bus.0.borrow().calls, fail_at);
            assert_eq!(
                error.submission,
                if fail_at >= submit {
                    Submission::Indeterminate
                } else {
                    Submission::NotSubmitted
                }
            );
            if fail_at >= submit {
                assert_latched(&mut tx, &bus);
            } else {
                assert!(!tx.needs_reconciliation());
            }
        }
    }
}

#[test]
fn cancellation_before_each_io_is_distinct_from_cancellation_after_submit() {
    for omci in [false, true] {
        let count = if omci { 10 } else { 5 };
        let submit = if omci { 8 } else { 3 };
        for at in 1..=count {
            let (mut tx, bus) = session(if omci { omci_steps() } else { ploam_steps() }, 0);
            let mut checkpoint = cancel_at(at);
            let error = if omci {
                tx.send_omci_frame(0xabc, true, &OMCI, policy(2), &mut checkpoint)
            } else {
                tx.send_ploam(&PLOAM, policy(2), &mut checkpoint)
            }
            .unwrap_err();
            assert_eq!(error.source.kind(), io::ErrorKind::Interrupted);
            assert_eq!(bus.0.borrow().calls, at - 1);
            assert_eq!(
                error.submission,
                if at > submit {
                    Submission::Indeterminate
                } else {
                    Submission::NotSubmitted
                }
            );
            if at > submit {
                assert_latched(&mut tx, &bus);
            }
        }
    }
}

#[test]
fn polling_timeout_does_not_resubmit_or_read_past_budget() {
    for omci in [false, true] {
        let mut steps = if omci { omci_steps() } else { ploam_steps() };
        *steps.last_mut().unwrap() = if omci {
            read(0x924, 2)
        } else {
            read(0x40f, 0x100)
        };
        let (mut tx, bus) = session(steps, 0);
        let error = if omci {
            tx.send_omci_frame(0xabc, true, &OMCI, policy(2), &mut proceed)
        } else {
            tx.send_ploam(&PLOAM, policy(2), &mut proceed)
        }
        .unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::TimedOut);
        assert!(bus.0.borrow().steps.is_empty());
        assert_latched(&mut tx, &bus);
    }
}

#[test]
fn submitted_omci_can_finish_after_preparation_slice_expires_without_replay() {
    let (mut tx, bus) = session(omci_steps(), 0);
    let mut preparation_checks = 0;
    let mut completion_checks = 0;
    let mut expired = false;
    tx.send_omci_frame_with_phases(0xabc, true, &OMCI, policy(2), &mut |phase, _| {
        match phase {
            TxPhase::Preparing => {
                assert!(!expired, "preparation continued after its deadline");
                preparation_checks += 1;
                // The submit itself consumes the remaining preparation slice.
                expired = preparation_checks == 8;
            }
            TxPhase::Completing => {
                assert!(expired);
                completion_checks += 1;
            }
        }
        Ok(())
    })
    .unwrap();
    assert_eq!((preparation_checks, completion_checks), (8, 2));
    assert!(!tx.needs_reconciliation());
    assert!(bus.0.borrow().steps.is_empty());
    assert_eq!(bus.0.borrow().calls, 10, "must not resubmit the frame");
}

#[test]
fn invalid_omci_arguments_fail_before_io() {
    let (mut tx, bus) = session(vec![], 0);
    for (port, frame) in [(0x1000, vec![0; 8]), (1, vec![]), (1, vec![0; 1981])] {
        let error = tx
            .send_omci_frame(port, false, &frame, policy(1), &mut proceed)
            .unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.submission, Submission::NotSubmitted);
    }
    assert_eq!(bus.0.borrow().calls, 0);
}

#[test]
fn omci_configuration_mismatch_stops_before_buffer_writes() {
    let (mut tx, bus) = session(
        vec![
            read(0x924, 0),
            read(0x913, 0),
            write(0x913, &[0x81]),
            read(0x913, 0),
        ],
        0,
    );
    let error = tx
        .send_omci_frame(1, false, &[0; 8], policy(1), &mut proceed)
        .unwrap_err();
    assert_eq!(error.submission, Submission::NotSubmitted);
    assert!(error.source.to_string().contains("readback"));
    assert_eq!(bus.0.borrow().calls, 4);
}

#[test]
fn maximum_sdk_frame_fits_248_rows_and_preserves_selection_zero() {
    let frame = vec![0x5a; 1980];
    let mut steps = vec![read(0x924, 0), read(0x913, 0x81), read(0x913, 0x81)];
    for row in 0..247 {
        steps.push(memory(row, &[0x5a; 8]));
    }
    steps.extend([
        memory(247, &[0x5a, 0x5a, 0x5a, 0x5a, 0, 0, 0, 0]),
        write(0x916, &[0x00f79ffe]),
        write(0x916, &[0x00f79fff]),
        read(0x924, 0),
    ]);
    let (mut tx, bus) = session(steps, 0);
    tx.send_omci_frame(0xfff, false, &frame, policy(1), &mut proceed)
        .unwrap();
    assert!(bus.0.borrow().steps.is_empty());
    assert_eq!(bus.0.borrow().calls, 254);
}

#[test]
fn all_partial_row_lengths_are_zero_padded_without_stale_bytes() {
    for len in 1..=16 {
        let frame: Vec<u8> = (1..=len).collect();
        let mut steps = vec![read(0x924, 0), read(0x913, 0x81), read(0x913, 0x81)];
        for row in 0..usize::from(len).div_ceil(8) {
            let mut raw = [0; 8];
            for (offset, byte) in raw.iter_mut().enumerate() {
                *byte = frame.get(row * 8 + offset).copied().unwrap_or(0);
            }
            steps.push(memory(row as u8, &raw));
        }
        let control = u32::from(len) * 8192 + 2;
        steps.extend([
            write(0x916, &[control]),
            write(0x916, &[control + 1]),
            read(0x924, 0),
        ]);
        let (mut tx, bus) = session(steps, 0);
        tx.send_omci_frame(1, false, &frame, policy(1), &mut proceed)
            .unwrap();
        assert!(bus.0.borrow().steps.is_empty());
    }
}

#[test]
fn real_ram_status_uses_gpon_mask_and_returns_full_readback() {
    let (_, bus) = session(
        vec![read(0x31, 0x40), read(0x31, 0xbe), read(0x31, 0x3bf)],
        0,
    );
    let value = wait_ram_initialized(&mut Fsl::new(bus.clone()), policy(3), &mut proceed).unwrap();
    assert_eq!(value, 0x3bf); // Switch/XGPON bits are not required by this test.
    assert_eq!(bus.0.borrow().calls, 3);
    assert_eq!(RAM_INIT_STATUS, 0x31);
    assert_eq!(GPON_RAM_INIT_MASK, 0xbf);
}

#[test]
fn missing_each_required_ram_bit_times_out_instead_of_reporting_ready() {
    for bit in [0, 1, 2, 3, 4, 5, 7] {
        let (_, bus) = session(vec![read(0x31, 0x3ff & !(1 << bit))], 0);
        let error =
            wait_ram_initialized(&mut Fsl::new(bus.clone()), policy(1), &mut proceed).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(bus.0.borrow().calls, 1);
    }
}

#[test]
fn ram_status_propagates_read_error_and_cancellation_without_retry() {
    let (_, bus) = session(vec![read(0x31, 0xbf)], 1);
    assert_eq!(
        wait_ram_initialized(&mut Fsl::new(bus.clone()), policy(200), &mut proceed)
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(bus.0.borrow().calls, 1);
    for at in 1..=2 {
        let (_, bus) = session(vec![read(0x31, 0), read(0x31, 0xbf)], 0);
        assert_eq!(
            wait_ram_initialized(&mut Fsl::new(bus.clone()), policy(200), &mut cancel_at(at))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(bus.0.borrow().calls, at - 1);
    }
}
