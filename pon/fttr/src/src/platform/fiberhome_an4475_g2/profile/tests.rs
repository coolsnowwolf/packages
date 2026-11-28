use super::*;
use crate::platform::fiberhome_an4475_g2::optical::Transfer;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

enum Response {
    Read { register: u8, data: Vec<u8> },
    Write { register: u8, data: Vec<u8> },
}

#[derive(Default)]
struct Script {
    responses: VecDeque<Response>,
    calls: usize,
    fail_at: usize,
}

#[derive(Clone)]
struct Mock(Rc<RefCell<Script>>);

impl Transfer for Mock {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> io::Result<usize> {
        assert_eq!(address, UX3326S_DEVICE_ADDRESS);
        let mut script = self.0.borrow_mut();
        script.calls += 1;
        let response = script.responses.pop_front().expect("unexpected I2C access");
        match response {
            Response::Read { register, data } => {
                assert_eq!(write, [register]);
                assert_eq!(read.len(), data.len());
                read.copy_from_slice(&data);
                if script.calls == script.fail_at {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "injected read error",
                    ));
                }
                Ok(2)
            }
            Response::Write { register, data } => {
                assert!(read.is_empty());
                assert_eq!(write, [&[register][..], data.as_slice()].concat());
                if script.calls == script.fail_at {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "injected write error",
                    ));
                }
                Ok(1)
            }
        }
    }
}

#[derive(Default)]
struct Pacer {
    calls: usize,
    wrong_iterations: bool,
}

impl StockPacer for Pacer {
    fn after_write(&mut self, loop_iterations: u32) -> io::Result<()> {
        self.calls += 1;
        self.wrong_iterations |= loop_iterations != STOCK_DELAY_ITERATIONS;
        Ok(())
    }
}

fn mock(responses: Vec<Response>, fail_at: usize) -> (OpticalBus<Mock>, Rc<RefCell<Script>>) {
    let script = Rc::new(RefCell::new(Script {
        responses: responses.into(),
        calls: 0,
        fail_at,
    }));
    (OpticalBus::new(Mock(script.clone())), script)
}

fn fixture(chip: u8) -> Vec<u8> {
    let mut bytes = vec![0; super::super::calibration::TABLE_BYTES];
    bytes[0x30] = chip;
    bytes[0x70..0x74].copy_from_slice(&0x780_u32.to_le_bytes());
    bytes[0x78..0x7c].copy_from_slice(&0x80_u32.to_le_bytes());
    for (index, byte) in bytes[0x80..].iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let payload_crc = crc32(&bytes[0x80..], 0);
    bytes[0x74..0x78].copy_from_slice(&payload_crc.to_le_bytes());
    let header_crc = crc32(&bytes[..0x7c], 0);
    bytes[0x7c..0x80].copy_from_slice(&header_crc.to_le_bytes());
    bytes
}

fn crc32(bytes: &[u8], seed: u32) -> u32 {
    let mut crc = !seed;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn identify_responses(signature: &[u8]) -> Vec<Response> {
    vec![
        Response::Read {
            register: 0,
            data: vec![0],
        },
        Response::Write {
            register: UX3326S_PAGE_REGISTER,
            data: vec![0x80],
        },
        Response::Read {
            register: UX3326S_SIGNATURE_REGISTER,
            data: signature.to_vec(),
        },
    ]
}

#[test]
fn live_probe_checks_exact_signature_and_reports_page_side_effect() {
    let (mut bus, script) = mock(identify_responses(UX3326S_SIGNATURE), 0);
    let chip = identify_ux3326s(&mut bus).unwrap();
    assert_eq!(chip.discriminator(), 0x27);
    assert_eq!(chip.current_page(), 0x80);
    assert_eq!(script.borrow().calls, 3);
}

#[test]
fn failed_probe_stops_without_fallback_or_rollback() {
    let (mut bus, script) = mock(identify_responses(b"OTHER123"), 0);
    assert_eq!(
        identify_ux3326s(&mut bus).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!bus.needs_reconciliation());
    assert_eq!(script.borrow().calls, 3);

    let (mut bus, script) = mock(identify_responses(UX3326S_SIGNATURE), 2);
    assert!(identify_ux3326s(&mut bus).is_err());
    assert!(bus.needs_reconciliation());
    assert_eq!(script.borrow().calls, 2);
}

#[test]
fn profile_plan_matches_observed_page_and_payload_ranges() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let writes = ux3326s_writes(table.payload()).unwrap();
    assert_eq!(writes.len(), 802);
    assert_eq!(
        &writes[..4],
        &[
            RegisterWrite {
                register: 0x7b,
                value: 0x66
            },
            RegisterWrite {
                register: 0x7c,
                value: 0x99
            },
            RegisterWrite {
                register: 0x7d,
                value: 0x55
            },
            RegisterWrite {
                register: 0x7e,
                value: 0xaa
            },
        ]
    );
    assert_eq!(
        writes[4],
        RegisterWrite {
            register: 0,
            value: 0
        }
    );
    assert_eq!(
        writes[80],
        RegisterWrite {
            register: 0x7f,
            value: 0x80
        }
    );
    assert_eq!(
        writes[81],
        RegisterWrite {
            register: 0x80,
            value: 5
        }
    );
    assert_eq!(
        writes[157],
        RegisterWrite {
            register: 0x7f,
            value: 0x81
        }
    );
    assert_eq!(
        writes[158],
        RegisterWrite {
            register: 0x80,
            value: 133
        }
    );
    assert_eq!(
        writes[286],
        RegisterWrite {
            register: 0x7f,
            value: 0x82
        }
    );
    assert_eq!(
        writes[415],
        RegisterWrite {
            register: 0x7f,
            value: 0x83
        }
    );
    assert_eq!(
        writes[544],
        RegisterWrite {
            register: 0x7f,
            value: 0x84
        }
    );
    assert_eq!(
        writes[545],
        RegisterWrite {
            register: 0x80,
            value: table.payload()[0x300]
        }
    );
    assert_eq!(
        writes[673],
        RegisterWrite {
            register: 0x7f,
            value: 0x85
        }
    );
    assert_eq!(
        writes[674],
        RegisterWrite {
            register: 0x80,
            value: table.payload()[0x380]
        }
    );
    assert_eq!(
        writes[801],
        RegisterWrite {
            register: 0xff,
            value: table.payload()[0x3ff]
        }
    );
    assert_eq!(STOCK_DELAY_ITERATIONS, 133_600);
}

#[test]
fn profile_requires_live_identity_and_stops_at_first_write_error() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let mut responses = identify_responses(UX3326S_SIGNATURE);
    responses.extend(
        ux3326s_writes(table.payload())
            .unwrap()
            .into_iter()
            .take(4)
            .map(|w| Response::Write {
                register: w.register,
                data: vec![w.value],
            }),
    );
    let (mut bus, script) = mock(responses, 5);
    let chip = identify_ux3326s(&mut bus).unwrap();
    let mut pacer = Pacer::default();
    assert!(program_ux3326s(&mut bus, chip, &table, &mut pacer, || Ok(())).is_err());
    assert_eq!(script.borrow().calls, 5);
    assert_eq!(pacer.calls, 1);
    assert!(bus.needs_reconciliation());
}

#[test]
fn profile_checks_cancel_between_each_transfer_and_delay() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let mut responses = identify_responses(UX3326S_SIGNATURE);
    responses.extend(
        ux3326s_writes(table.payload())
            .unwrap()
            .iter()
            .take(1)
            .map(|w| Response::Write {
                register: w.register,
                data: vec![w.value],
            }),
    );
    let (mut bus, script) = mock(responses, 0);
    let chip = identify_ux3326s(&mut bus).unwrap();
    let mut pacer = Pacer::default();
    let mut checkpoints = 0;
    let error = program_ux3326s(&mut bus, chip, &table, &mut pacer, || {
        checkpoints += 1;
        if checkpoints == 2 {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(script.borrow().calls, 4);
    assert_eq!(pacer.calls, 0);
    assert!(bus.needs_reconciliation());
}

#[test]
fn profile_completes_with_checked_single_byte_writes() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let mut responses = identify_responses(UX3326S_SIGNATURE);
    responses.extend(
        ux3326s_writes(table.payload())
            .unwrap()
            .into_iter()
            .map(|w| Response::Write {
                register: w.register,
                data: vec![w.value],
            }),
    );
    let (mut bus, script) = mock(responses, 0);
    let chip = identify_ux3326s(&mut bus).unwrap();
    let mut pacer = Pacer::default();
    program_ux3326s(&mut bus, chip, &table, &mut pacer, || Ok(())).unwrap();
    assert_eq!(script.borrow().calls, 3 + 802);
    assert_eq!(pacer.calls, 802);
    assert!(!pacer.wrong_iterations);
    assert!(!bus.needs_reconciliation());
}

#[test]
fn initializer_rejects_wrong_persisted_branch_before_probing() {
    let bytes = fixture(0x26);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let (mut bus, script) = mock(Vec::new(), 0);
    let mut pacer = Pacer::default();
    let error = initialize_ux3326s(&mut bus, &table, &mut pacer, || Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(script.borrow().calls, 0);
}

#[test]
fn initializer_probes_then_applies_selected_branch() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let mut responses = identify_responses(UX3326S_SIGNATURE);
    responses.extend(
        ux3326s_writes(table.payload())
            .unwrap()
            .into_iter()
            .map(|w| Response::Write {
                register: w.register,
                data: vec![w.value],
            }),
    );
    let (mut bus, script) = mock(responses, 0);
    let mut pacer = Pacer::default();
    let chip = initialize_ux3326s(&mut bus, &table, &mut pacer, || Ok(())).unwrap();
    assert_eq!(chip.discriminator(), UX3326S_CHIP_ID);
    assert_eq!(script.borrow().calls, 3 + 802);
    assert_eq!(pacer.calls, 802);
}

#[test]
fn independent_expected_sequence_matches_stock_ux3326s_schedule() {
    let bytes = fixture(0x27);
    let table = CalibrationTable::parse(&bytes).unwrap();
    let payload = table.payload();

    let mut expected = Vec::new();
    for (reg, val) in [(0x7b, 0x66), (0x7c, 0x99), (0x7d, 0x55), (0x7e, 0xaa)] {
        expected.push(RegisterWrite {
            register: reg,
            value: val,
        });
    }
    for i in 0..0x4c_usize {
        expected.push(RegisterWrite {
            register: i as u8,
            value: payload[i],
        });
    }
    expected.push(RegisterWrite {
        register: 0x7f,
        value: 0x80,
    });
    for i in 0..0x4c_usize {
        expected.push(RegisterWrite {
            register: (0x80 + i) as u8,
            value: payload[0x100 + i],
        });
    }
    for (page_idx, page) in (0x81_u8..=0x85).enumerate() {
        expected.push(RegisterWrite {
            register: 0x7f,
            value: page,
        });
        let base = 0x180 + page_idx * 0x80;
        for i in 0..0x80_usize {
            expected.push(RegisterWrite {
                register: (0x80 + i) as u8,
                value: payload[base + i],
            });
        }
    }

    assert_eq!(expected.len(), 802);
    let actual = ux3326s_writes(payload).unwrap();
    assert_eq!(actual, expected);
}
