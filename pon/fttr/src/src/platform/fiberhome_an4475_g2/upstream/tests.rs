use super::*;
use crate::platform::fiberhome_an4475_g2::{
    downstream::Submission, protocol::Fsl, test_support::*,
};

fn rx_session(steps: Vec<Step>, fail_at: usize, cursor: u8) -> (Management<Mock>, Mock) {
    let (_, bus) = session(steps, fail_at);
    (
        Management::with_omci_cursor(Fsl::new(bus.clone()), cursor),
        bus,
    )
}

fn ploam_data() -> Step {
    let words: [u32; 13] = [
        0x02468ad7, 0x0a0b0c0d, 0x06070809, 0x02030405, 1, 0x15161718, 0x191a1b1c, 0x1d1e1f20,
        0x21222324, 0x25262728, 0x292a2b2c, 0x2d2e2f30, 0x31323334,
    ];
    message(
        [0x22, 0x90, 0xc0, 0, 0],
        &[0],
        words.iter().flat_map(|v| v.to_be_bytes()).collect(),
    )
}

fn ploam_steps(last: bool) -> Vec<Step> {
    let mut steps = vec![
        read(0x41b, 1),
        read(0x41c, u32::from(last)),
        ploam_data(),
        write(0x41b, &[0]),
    ];
    if last {
        steps.push(write(0x41c, &[0]));
    }
    steps
}

fn row(row: u8, bytes: [u8; 8]) -> Step {
    message([0x4e, 0x55, 0x10, row, 0], &[0], bytes.to_vec())
}

fn header(row_index: u8, metadata: u32) -> Step {
    let mut bytes = [0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0];
    bytes[4..].copy_from_slice(&metadata.to_be_bytes());
    row(row_index, bytes)
}

fn omci_steps() -> Vec<Step> {
    vec![
        read(0x924, 3),
        read(0x917, 0x12345678),
        header(254, 0x802ac00b),
        row(255, [1, 2, 3, 4, 5, 6, 7, 8]),
        row(0, [9, 10, 11, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe]),
        write(0x921, &[1]),
    ]
}

fn assert_all_latched(rx: &mut Management<Mock>, bus: &Mock) {
    assert!(rx.needs_reconciliation());
    let calls = bus.0.borrow().calls;
    for error in [
        rx.receive_ploam(policy(1), &mut proceed).unwrap_err(),
        rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err(),
    ] {
        assert_eq!(error.consumption, Consumption::Indeterminate);
        assert!(error.frame.is_none());
    }
    assert_eq!(
        rx.send_ploam(&[0; 12], policy(1), &mut proceed)
            .unwrap_err()
            .submission,
        Submission::Indeterminate
    );
    assert_eq!(
        rx.send_omci_frame(1, false, &[0; 8], policy(1), &mut proceed)
            .unwrap_err()
            .submission,
        Submission::Indeterminate
    );
    assert_eq!(bus.0.borrow().calls, calls);
}

#[test]
fn ploam_preserves_416_bits_and_stock_byte_layout_then_clears_valid_and_last() {
    for last in [false, true] {
        let (mut rx, bus) = rx_session(ploam_steps(last), 0, 0);
        let record = rx.receive_ploam(policy(1), &mut proceed).unwrap().unwrap();
        assert_eq!(record.frame, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
        assert_eq!(record.last, last);
        assert_eq!(record.words[0], 0x02468ad7);
        assert_eq!(record.words[12], 0x31323334);
        assert!(record.stock_error_flag());
        assert!(bus.0.borrow().steps.is_empty());
        assert!(!rx.needs_reconciliation());
    }
}

#[test]
fn ploam_error_flag_is_bit4_not_other_metadata_fields() {
    for bit in 0..32 {
        let mut words = [0; 13];
        words[0] = 1 << bit;
        assert_eq!(
            PloamRecord::decode(words, false).stock_error_flag(),
            bit == 4
        );
    }
}

#[test]
fn empty_polls_are_bounded_and_never_consume_even_when_last_is_set() {
    let (mut rx, bus) = rx_session(
        vec![
            read(0x41b, 0),
            read(0x41c, 1),
            read(0x41b, 0),
            read(0x41c, 1),
        ],
        0,
        254,
    );
    let mut delays = Vec::new();
    assert!(rx
        .receive_ploam(policy(2), &mut |d| {
            delays.push(d);
            Ok(())
        })
        .unwrap()
        .is_none());
    assert_eq!(
        delays,
        [
            Duration::ZERO,
            Duration::ZERO,
            policy(2).interval,
            Duration::ZERO
        ]
    );
    bus.0
        .borrow_mut()
        .steps
        .extend([read(0x924, 2), read(0x924, 0)]);
    delays.clear();
    assert!(rx
        .receive_omci_frame(policy(2), &mut |d| {
            delays.push(d);
            Ok(())
        })
        .unwrap()
        .is_none());
    assert_eq!(delays, [Duration::ZERO, policy(2).interval]);
    assert_eq!(rx.omci_cursor(), Some(254));
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn delayed_valid_is_read_then_consumed_once() {
    let mut steps = vec![read(0x41b, 0), read(0x41c, 0)];
    steps.extend(ploam_steps(false));
    let (mut rx, bus) = rx_session(steps, 0, 0);
    assert!(rx.receive_ploam(policy(2), &mut proceed).unwrap().is_some());
    assert_eq!(bus.0.borrow().calls, 6);
}

#[test]
fn omci_wraps_ring_decodes_header_and_drops_only_tail_padding() {
    let (mut rx, bus) = rx_session(omci_steps(), 0, 254);
    let frame = rx
        .receive_omci_frame(policy(1), &mut proceed)
        .unwrap()
        .unwrap();
    assert_eq!(frame.metadata, 0x802ac00b);
    assert_eq!(frame.source, 0xab);
    assert_eq!(frame.kind, OmciKind::Omci);
    assert_eq!(frame.payload, (1..=11).collect::<Vec<_>>());
    assert_eq!(rx.omci_cursor(), Some(1));
    assert!(bus.0.borrow().steps.is_empty());
    // A subsequent call starts at the committed cursor, not at the old header.
    bus.0.borrow_mut().steps.extend([
        read(0x924, 1),
        read(0x917, 0x12345678),
        header(1, 1),
        row(2, [0x5a; 8]),
        write(0x921, &[1]),
    ]);
    let frame = rx
        .receive_omci_frame(policy(1), &mut proceed)
        .unwrap()
        .unwrap();
    assert_eq!(frame.kind, OmciKind::Wmci);
    assert_eq!(frame.source, 0);
    assert_eq!(frame.payload, [0x5a]);
    assert_eq!(rx.omci_cursor(), Some(3));
}

#[test]
fn receive_requires_explicit_initial_omci_cursor() {
    let (mut rx, bus) = session(vec![], 0);
    let error = rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err();
    assert_eq!(error.source.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(error.consumption, Consumption::NotAttempted);
    assert!(error.frame.is_none());
    assert_eq!(rx.omci_cursor(), None);
    assert_eq!(bus.0.borrow().calls, 0);
}

#[test]
fn every_rx_io_failure_stops_without_advancing_cursor_or_replaying_consume() {
    for omci in [false, true] {
        let count = if omci { 6 } else { 5 };
        let consume = if omci { 6 } else { 4 };
        for at in 1..=count {
            let (mut rx, bus) = rx_session(
                if omci {
                    omci_steps()
                } else {
                    ploam_steps(true)
                },
                at,
                254,
            );
            let error = if omci {
                rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err()
            } else {
                rx.receive_ploam(policy(1), &mut proceed).unwrap_err()
            };
            assert_eq!(error.source.kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(
                error.consumption,
                if at >= consume {
                    Consumption::Indeterminate
                } else {
                    Consumption::NotAttempted
                }
            );
            assert_eq!(error.frame.is_some(), at >= consume);
            if at >= consume {
                match error.frame.as_ref().unwrap() {
                    ReceivedFrame::Ploam(record) => {
                        assert_eq!(record.frame, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13])
                    }
                    ReceivedFrame::Omci(frame) => {
                        assert_eq!(frame.payload, (1..=11).collect::<Vec<_>>())
                    }
                }
            }
            assert_eq!(rx.omci_cursor(), Some(254));
            assert_eq!(bus.0.borrow().calls, at);
            if at >= consume {
                assert_all_latched(&mut rx, &bus);
            }
        }
    }
}

#[test]
fn cancellation_at_every_access_retains_complete_frame_but_never_commits_cursor() {
    for omci in [false, true] {
        let count = if omci { 6 } else { 5 };
        let consume = if omci { 6 } else { 4 };
        for at in 1..=count {
            let (mut rx, bus) = rx_session(
                if omci {
                    omci_steps()
                } else {
                    ploam_steps(true)
                },
                0,
                254,
            );
            let error = if omci {
                rx.receive_omci_frame(policy(1), &mut cancel_at(at))
                    .unwrap_err()
            } else {
                rx.receive_ploam(policy(1), &mut cancel_at(at)).unwrap_err()
            };
            assert_eq!(error.source.kind(), io::ErrorKind::Interrupted);
            assert_eq!(
                error.consumption,
                if at > consume {
                    Consumption::Indeterminate
                } else {
                    Consumption::NotAttempted
                }
            );
            assert_eq!(error.frame.is_some(), at >= consume);
            assert_eq!(rx.omci_cursor(), Some(254));
            assert_eq!(bus.0.borrow().calls, at - 1);
            if at > consume {
                assert_all_latched(&mut rx, &bus);
            }
        }
    }
}

#[test]
fn ambiguous_tx_also_blocks_both_receive_paths() {
    let (mut rx, bus) = rx_session(
        vec![
            read(0x40f, 0),
            write(0x403, &[0; 12]),
            write(0x40f, &[0x101]),
        ],
        3,
        0,
    );
    rx.send_ploam(&[0; 12], policy(1), &mut proceed)
        .unwrap_err();
    assert_all_latched(&mut rx, &bus);
}

#[test]
fn mismatched_header_scan_is_bounded_to_one_ring_and_keeps_original_cursor() {
    let mut steps = vec![read(0x924, 1), read(0x917, 0x12345678)];
    for i in 0..256 {
        steps.push(row(254u8.wrapping_add(i as u8), [0; 8]));
    }
    let (mut rx, bus) = rx_session(steps, 0, 254);
    let error = rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err();
    assert_eq!(error.source.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.consumption, Consumption::NotAttempted);
    assert_eq!(rx.omci_cursor(), Some(254));
    assert_eq!(bus.0.borrow().calls, 258);
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn bounded_scan_can_find_a_wrapped_header_without_early_cursor_commit() {
    let (mut rx, bus) = rx_session(
        vec![
            read(0x924, 1),
            read(0x917, 0x12345678),
            row(254, [0; 8]),
            row(255, [0; 8]),
            header(0, 1),
            row(1, [0x5a; 8]),
            write(0x921, &[1]),
        ],
        0,
        254,
    );
    assert_eq!(
        rx.receive_omci_frame(policy(1), &mut proceed)
            .unwrap()
            .unwrap()
            .payload,
        [0x5a]
    );
    assert_eq!(rx.omci_cursor(), Some(2));
    assert!(bus.0.borrow().steps.is_empty());
}

#[test]
fn invalid_lengths_never_acknowledge_or_advance() {
    for len in [0, 2001, 0x3fff] {
        let (mut rx, bus) = rx_session(
            vec![read(0x924, 1), read(0x917, 0x12345678), header(254, len)],
            0,
            254,
        );
        let error = rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err();
        assert_eq!(error.source.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.consumption, Consumption::NotAttempted);
        assert_eq!(rx.omci_cursor(), Some(254));
        assert_eq!(bus.0.borrow().calls, 3);
    }
}

#[test]
fn every_partial_row_length_and_maximum_raw_frame_use_exact_length_and_wrapping() {
    for len in (1usize..=16).chain([1980, 2000]) {
        let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut steps = vec![
            read(0x924, 1),
            read(0x917, 0x12345678),
            header(254, len as u32),
        ];
        for (i, chunk) in payload.chunks(8).enumerate() {
            let mut bytes = [0xfa; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            steps.push(row(255u8.wrapping_add(i as u8), bytes));
        }
        steps.push(write(0x921, &[1]));
        let (mut rx, bus) = rx_session(steps, 0, 254);
        assert_eq!(
            rx.receive_omci_frame(policy(1), &mut proceed)
                .unwrap()
                .unwrap()
                .payload,
            payload
        );
        assert_eq!(
            rx.omci_cursor(),
            Some(255u8.wrapping_add(len.div_ceil(8) as u8))
        );
        assert!(bus.0.borrow().steps.is_empty());
    }
}

#[test]
fn resynchronization_read_failure_or_cancellation_does_not_commit_skipped_rows() {
    for fail in [false, true] {
        let (mut rx, bus) = rx_session(
            vec![
                read(0x924, 1),
                read(0x917, 0x12345678),
                row(254, [0; 8]),
                header(255, 1),
            ],
            if fail { 4 } else { 0 },
            254,
        );
        let error = if fail {
            rx.receive_omci_frame(policy(1), &mut proceed).unwrap_err()
        } else {
            rx.receive_omci_frame(policy(1), &mut cancel_at(4))
                .unwrap_err()
        };
        assert_eq!(
            error.source.kind(),
            if fail {
                io::ErrorKind::BrokenPipe
            } else {
                io::ErrorKind::Interrupted
            }
        );
        assert_eq!(error.consumption, Consumption::NotAttempted);
        assert!(error.frame.is_none());
        assert_eq!(rx.omci_cursor(), Some(254));
        assert!(!rx.needs_reconciliation());
        assert_eq!(bus.0.borrow().calls, if fail { 4 } else { 3 });
    }
}

#[test]
fn empty_poll_io_errors_are_not_misreported_as_an_empty_queue() {
    let (mut rx, bus) = rx_session(vec![read(0x924, 0)], 1, 0);
    assert_eq!(
        rx.receive_omci_frame(policy(1), &mut proceed)
            .unwrap_err()
            .source
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(bus.0.borrow().calls, 1);
    let (mut rx, bus) = rx_session(vec![read(0x41b, 0), read(0x41c, 0), read(0x41b, 1)], 0, 0);
    assert_eq!(
        rx.receive_ploam(policy(2), &mut cancel_at(3))
            .unwrap_err()
            .source
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(bus.0.borrow().calls, 2);
}
