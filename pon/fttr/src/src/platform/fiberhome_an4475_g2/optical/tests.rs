use super::*;
use std::{cell::Cell, rc::Rc};

struct Mock {
    calls: Rc<Cell<usize>>,
    write: bool,
    len: usize,
    result: io::Result<usize>,
}

impl Transfer for Mock {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> io::Result<usize> {
        assert_eq!(
            self.calls.replace(self.calls.get() + 1),
            0,
            "unexpected replay/readback"
        );
        assert_eq!(address, 0x63);
        if self.write {
            assert!(read.is_empty());
            assert_eq!(write[0], 0x7f);
            assert_eq!(&write[1..], &(1..=self.len as u8).collect::<Vec<_>>());
        } else {
            assert_eq!(write, [0x60]);
            assert_eq!(read.len(), self.len);
            for (index, byte) in read.iter_mut().enumerate() {
                *byte = (index + 1) as u8;
            }
        }
        std::mem::replace(
            &mut self.result,
            Err(io::Error::other("unexpected transfer")),
        )
    }
}

fn bus(write: bool, len: usize, result: io::Result<usize>) -> (OpticalBus<Mock>, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    (
        OpticalBus::new(Mock {
            calls: calls.clone(),
            write,
            len,
            result,
        }),
        calls,
    )
}

#[test]
fn single_message_write_all_lengths_no_readback_or_split() {
    for len in 1..=MAX_DATA_BYTES {
        let (mut bus, calls) = bus(true, len, Ok(1));
        bus.write(0x63, 0x7f, &(1..=len as u8).collect::<Vec<_>>())
            .unwrap();
        assert!(!bus.needs_reconciliation());
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn combined_pointer_read_all_lengths() {
    for len in 1..=MAX_DATA_BYTES {
        let (mut bus, calls) = bus(false, len, Ok(2));
        let mut output = vec![0xaa; len];
        bus.read(0x63, 0x60, &mut output).unwrap();
        assert_eq!(output, (1..=len as u8).collect::<Vec<_>>());
        assert!(!bus.needs_reconciliation());
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn short_or_failed_reads_do_not_publish_partial_data_or_retry() {
    for result in [
        Ok(0),
        Ok(1),
        Ok(3),
        Err(io::Error::from_raw_os_error(libc::EINTR)),
        Err(io::Error::from_raw_os_error(libc::EIO)),
    ] {
        let (mut bus, calls) = bus(false, 16, result);
        let mut output = [0xaa; 16];
        assert!(bus.read(0x63, 0x60, &mut output).is_err());
        assert_eq!(output, [0xaa; 16]);
        assert_eq!(calls.get(), 1);
        assert!(!bus.needs_reconciliation());
    }
}

#[test]
fn short_or_failed_writes_latch_both_paths_and_preserve_errno() {
    for result in [
        Ok(0),
        Ok(2),
        Err(io::Error::from_raw_os_error(libc::EINTR)),
        Err(io::Error::from_raw_os_error(libc::EREMOTEIO)),
    ] {
        let expected_errno = result.as_ref().err().and_then(io::Error::raw_os_error);
        let (mut bus, calls) = bus(true, 1, result);
        let error = bus.write(0x63, 0x7f, &[1]).unwrap_err();
        assert_eq!(error.raw_os_error(), expected_errno);
        assert!(bus.needs_reconciliation());
        assert!(bus.write(0x63, 0x7f, &[1]).is_err());
        assert!(bus.read(0x63, 0x60, &mut [0; 1]).is_err());
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn invalid_lengths_and_non_seven_bit_addresses_have_no_io() {
    let (mut bus, calls) = bus(false, 1, Ok(2));
    for address in [0x80, 0xa2, 0xff] {
        assert_eq!(
            bus.read(address, 0, &mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(bus.write(address, 0, &[1]).is_err());
    }
    for len in [0, 17, 256] {
        assert!(bus.read(0x63, 0, &mut vec![0; len]).is_err());
        assert!(bus.write(0x63, 0, &vec![0; len]).is_err());
    }
    assert_eq!(calls.get(), 0);
    assert!(!bus.needs_reconciliation());
}
