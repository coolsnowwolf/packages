use super::{
    management::Management,
    polling::PollPolicy,
    protocol::{crc8, Bus, Fsl},
};
use std::{cell::RefCell, collections::VecDeque, io, num::NonZeroU16, rc::Rc, time::Duration};

pub(super) struct Step {
    pub tx: Vec<u8>,
    pub rx: Vec<u8>,
}

#[derive(Default)]
pub(super) struct Script {
    pub steps: VecDeque<Step>,
    pub calls: usize,
    pub fail_at: usize,
}

#[derive(Clone)]
pub(super) struct Mock(pub Rc<RefCell<Script>>);

impl Bus for Mock {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> io::Result<()> {
        let mut script = self.0.borrow_mut();
        script.calls += 1;
        let step = script.steps.pop_front().expect("unexpected SPI access");
        assert_eq!(tx, step.tx, "SPI message {}", script.calls);
        assert_eq!(rx.len(), step.rx.len());
        if script.calls == script.fail_at {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected SPI error",
            ));
        }
        rx.copy_from_slice(&step.rx);
        Ok(())
    }
}

pub(super) fn message(mut header: [u8; 5], data: &[u8], rx: Vec<u8>) -> Step {
    // CRC has independent stock vectors in protocol.rs. These scripts verify
    // transaction ordering and raw wire payloads, not just decoded word values.
    header[4] = crc8(&header[..4]);
    let mut tx = header.to_vec();
    tx.extend_from_slice(data);
    Step { tx, rx }
}

pub(super) fn read(address: u32, value: u32) -> Step {
    message(
        [((address >> 8) as u8) << 3 | 2, address as u8, 0, 0, 0],
        &[0],
        value.to_be_bytes().to_vec(),
    )
}

pub(super) fn write(address: u32, words: &[u32]) -> Step {
    let data: Vec<u8> = words.iter().flat_map(|v| v.to_be_bytes()).collect();
    message(
        [
            ((address >> 8) as u8) << 3 | 1,
            address as u8,
            ((words.len() - 1) as u8) << 4,
            0,
            0,
        ],
        &data,
        vec![],
    )
}

pub(super) fn memory(row: u8, wire_bytes: &[u8; 8]) -> Step {
    message([0x4d, 0x56, 0x10, row, 0], wire_bytes, vec![])
}

pub(super) fn session(steps: Vec<Step>, fail_at: usize) -> (Management<Mock>, Mock) {
    let bus = Mock(Rc::new(RefCell::new(Script {
        steps: steps.into(),
        fail_at,
        ..Script::default()
    })));
    (Management::new(Fsl::new(bus.clone())), bus)
}

pub(super) fn policy(reads: u16) -> PollPolicy {
    PollPolicy {
        max_reads: NonZeroU16::new(reads).unwrap(),
        interval: Duration::from_micros(150),
    }
}

pub(super) fn proceed(_: Duration) -> io::Result<()> {
    Ok(())
}

pub(super) fn cancel_at(at: usize) -> impl FnMut(Duration) -> io::Result<()> {
    let mut checks = 0;
    move |_| {
        checks += 1;
        if checks == at {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Ok(())
        }
    }
}
