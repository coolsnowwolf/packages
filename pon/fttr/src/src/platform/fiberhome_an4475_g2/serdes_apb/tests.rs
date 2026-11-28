use super::*;
use crate::platform::fiberhome_an4475_g2::test_support::{
    message, read, write, Mock, Script, Step,
};
use std::{cell::RefCell, collections::VecDeque, io, rc::Rc};

fn session(steps: Vec<Step>, fail_at: usize) -> (SerdesApb<Mock>, Mock) {
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: VecDeque::from(steps),
        fail_at,
        ..Script::default()
    })));
    (SerdesApb::new(Fsl::new(bus.clone()), 1).unwrap(), bus)
}

fn apb_read_status(value: u32) -> Step {
    read(APB_CTRL, value)
}

fn apb_read_data(value: u32) -> Step {
    read(APB_RDATA, value)
}

#[test]
fn read_uses_selected_bus_command_ack_and_data_in_order() {
    let (mut apb, bus) = session(
        vec![
            write(BUS_SELECT, &[1]),
            read(APB_CTRL, 0),
            write(APB_CTRL, &[0x1234]),
            apb_read_status(APB_ACK),
            apb_read_data(0xfeed_12ab),
        ],
        0,
    );
    assert_eq!(apb.read(0x1234, &mut |_| Ok(())).unwrap(), 0xab);
    assert!(!apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 5);
}

#[test]
fn write_sets_write_bit_and_stops_on_ack_error() {
    let (mut apb, bus) = session(
        vec![
            write(BUS_SELECT, &[1]),
            write(APB_WDATA, &[0x5a]),
            read(APB_CTRL, 0),
            write(APB_CTRL, &[APB_WRITE | 0x4321]),
            apb_read_status(APB_ACK | APB_ERROR),
        ],
        0,
    );
    assert!(apb.write(0x4321, 0x5a, &mut |_| Ok(())).is_err());
    assert!(apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 5);
    assert!(apb.read(0x4321, &mut |_| Ok(())).is_err());
    assert_eq!(bus.0.borrow().calls, 5);
}

#[test]
fn read_errors_and_apb_target_error_latch_without_fetching_data() {
    for fail_at in 1..=5 {
        let steps = vec![
            write(BUS_SELECT, &[1]),
            read(APB_CTRL, 0),
            write(APB_CTRL, &[0x1234]),
            apb_read_status(APB_ACK),
            apb_read_data(0x42),
        ];
        let (mut apb, bus) = session(steps, fail_at);
        assert!(apb.read(0x1234, &mut |_| Ok(())).is_err());
        assert!(apb.needs_reconciliation());
        assert_eq!(bus.0.borrow().calls, fail_at);
        assert!(apb.read(0x1234, &mut |_| Ok(())).is_err());
        assert_eq!(bus.0.borrow().calls, fail_at);
    }

    let (mut apb, bus) = session(
        vec![
            write(BUS_SELECT, &[1]),
            read(APB_CTRL, 0),
            write(APB_CTRL, &[0x1234]),
            apb_read_status(APB_ACK | APB_ERROR),
        ],
        0,
    );
    assert!(apb.read(0x1234, &mut |_| Ok(())).is_err());
    assert!(apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 4);
}

#[test]
fn ack_poll_is_bounded_and_cancellation_after_submit_latches_session() {
    let mut steps = vec![
        write(BUS_SELECT, &[1]),
        write(APB_WDATA, &[0xaa]),
        read(APB_CTRL, 0),
        write(APB_CTRL, &[APB_WRITE | 0x10]),
    ];
    steps.push(apb_read_status(0));
    steps.extend((0..APB_ACK_READS).map(|_| apb_read_status(0)));
    let (mut apb, bus) = session(steps, 0);
    let mut waits = 0;
    let err = apb
        .write(0x10, 0xaa, &mut |delay| {
            if delay == APB_ACK_INTERVAL {
                waits += 1;
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert_eq!(waits, APB_ACK_READS as usize - 1);
    assert!(apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 4 + APB_ACK_READS as usize);

    let mut steps = vec![
        write(BUS_SELECT, &[1]),
        write(APB_WDATA, &[0xaa]),
        read(APB_CTRL, 0),
        write(APB_CTRL, &[APB_WRITE | 0x10]),
    ];
    steps.push(apb_read_status(APB_ACK));
    let (mut apb, bus) = session(steps, 0);
    let mut zero_checkpoints = 0;
    let err = apb
        .write(0x10, 0xaa, &mut |delay| {
            if delay.is_zero() {
                zero_checkpoints += 1;
            }
            if zero_checkpoints == 4 {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    assert!(apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 4);
}

#[test]
fn spi_errors_and_precommand_cancel_are_not_replayed() {
    for fail_at in 1..=5 {
        let steps = vec![
            write(BUS_SELECT, &[1]),
            write(APB_WDATA, &[0x33]),
            read(APB_CTRL, 0),
            write(APB_CTRL, &[APB_WRITE | 7]),
            apb_read_status(APB_ACK),
        ];
        let (mut apb, bus) = session(steps, fail_at);
        assert!(apb.write(7, 0x33, &mut |_| Ok(())).is_err());
        assert!(apb.needs_reconciliation());
        assert_eq!(bus.0.borrow().calls, fail_at);
        assert!(apb.write(7, 0x33, &mut |_| Ok(())).is_err());
        assert_eq!(bus.0.borrow().calls, fail_at);
    }

    let (mut apb, bus) = session(vec![], 0);
    assert_eq!(
        apb.write(0, 0, &mut |_| Err(io::Error::from(
            io::ErrorKind::Interrupted
        )))
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!apb.needs_reconciliation());
    assert_eq!(bus.0.borrow().calls, 0);
}

#[test]
fn bus_selector_bounds_are_checked_before_any_io() {
    let bus = Mock(Rc::new(RefCell::new(Script::default())));
    assert!(matches!(
        SerdesApb::new(Fsl::new(bus.clone()), 4),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput
    ));
    assert_eq!(bus.0.borrow().calls, 0);
}

#[test]
fn shared_apb_control_register_shape_matches_descriptor() {
    let expected = vec![
        write(BUS_SELECT, &[1]),
        write(APB_WDATA, &[0x5a]),
        read(APB_CTRL, 0),
        message(
            [((APB_CTRL >> 8) as u8) << 3 | 1, APB_CTRL as u8, 0, 0, 0],
            &(APB_WRITE | 0x1234).to_be_bytes(),
            vec![],
        ),
    ];
    let (mut apb, bus) = session(expected, 4);
    assert!(apb.write(0x1234, 0x5a, &mut |_| Ok(())).is_err());
    assert_eq!(bus.0.borrow().calls, 4);
}

#[test]
fn set_bus_and_borrowed_helpers_target_expected_bus() {
    let expected = vec![
        write(BUS_SELECT, &[2]),
        read(APB_CTRL, 0),
        write(APB_CTRL, &[0x30]),
        read(APB_CTRL, APB_ACK),
        read(APB_RDATA, 0xab),
    ];
    let (mut apb, bus) = session(expected, usize::MAX);
    apb.set_bus(2).unwrap();
    assert_eq!(apb.bus(), 2);
    assert!(matches!(
        apb.set_bus(4),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput
    ));
    let mut fsl = apb.into_inner();
    assert_eq!(read_on(&mut fsl, 2, 0x30, &mut |_| Ok(())).unwrap(), 0xab);
    assert_eq!(bus.0.borrow().calls, 5);
}
