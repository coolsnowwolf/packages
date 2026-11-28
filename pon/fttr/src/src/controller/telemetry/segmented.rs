//! Incremental automatic MIB upload for a serialized liveness worker.
use crate::pon_engine::omci::{mib::MibReport, session::MibUpload, OmciSession};
use std::{
    io,
    time::{Duration, Instant},
};

pub(super) struct Upload {
    session: OmciSession,
    upload: MibUpload,
    sync: u32,
    started: Instant,
}

fn mib_sync(session: &mut OmciSession) -> io::Result<u32> {
    session
        .get_attributes(2, 0, 0x8000)?
        .attributes
        .first()
        .and_then(|attribute| attribute.value.unsigned())
        .ok_or_else(|| io::Error::other("MIB data sync unavailable"))
}

impl Upload {
    pub fn begin(mut session: OmciSession, deadline: Instant) -> io::Result<Self> {
        session.finish_before(deadline);
        let sync = mib_sync(&mut session)?;
        let started = Instant::now();
        let upload = session.begin_mib_upload();
        Ok(Self {
            session,
            upload,
            sync,
            started,
        })
    }

    fn prepare(&mut self, deadline: Instant) -> io::Result<()> {
        if self.started.elapsed() >= Duration::from_secs(600) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MIB snapshot expired",
            ));
        }
        self.session
            .finish_before(deadline.min(self.started + Duration::from_secs(600)));
        Ok(())
    }

    /// A deadline timeout preserves the cursor; other errors invalidate the snapshot.
    pub fn step(&mut self, deadline: Instant) -> io::Result<bool> {
        self.prepare(deadline)?;
        loop {
            match self.session.next_mib_record(&mut self.upload) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(false),
                Err(error) => return Err(error),
            }
        }
    }

    pub fn verify(&mut self, deadline: Instant) -> io::Result<()> {
        self.prepare(deadline)?;
        // Exact comparison on the decoded value (byte-width MibDataSync,
        // schema size 1) is intentional: any counter movement inside the
        // window (including the 255→0 wrap) must reject the whole snapshot. The
        // only theoretical hole — exactly 256 increments landing back on the
        // observed byte — is indistinguishable at byte width, and a window with
        // 256 MIB changes deserves a re-upload anyway.
        if mib_sync(&mut self.session)? != self.sync {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MIB data sync changed during upload",
            ));
        }
        Ok(())
    }

    pub fn finish(self) -> MibReport {
        OmciSession::finish_mib_upload(self.upload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
    use std::sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    };

    struct Mock {
        pending: Option<BaselineFrame>,
        sync: Arc<AtomicU8>,
        sequences: Arc<Mutex<Vec<u16>>>,
        delay_type: u8,
        ready_at: Instant,
        sent: Arc<Mutex<Vec<BaselineFrame>>>,
    }

    impl OmciTransport for Mock {
        fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
            self.pending = Some(frame.clone());
            self.sent.lock().unwrap().push(*frame);
            self.ready_at = Instant::now()
                + if frame.message_type == self.delay_type {
                    Duration::from_millis(80)
                } else {
                    Duration::ZERO
                };
            Ok(())
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            if Instant::now() < self.ready_at {
                return Ok(None);
            }
            Ok(self.pending.take().map(|request| {
                let mut payload = [0; 32];
                match request.message_type {
                    0x49 => payload[..4].copy_from_slice(&[
                        0,
                        0x80,
                        0,
                        self.sync.load(Ordering::SeqCst),
                    ]),
                    0x4d => payload[..2].copy_from_slice(&2u16.to_be_bytes()),
                    0x4e => {
                        let seq = u16::from_be_bytes([request.payload[0], request.payload[1]]);
                        self.sequences.lock().unwrap().push(seq);
                        payload[..6].copy_from_slice(&[1, 7, 0, seq as u8, 0x80, 0]);
                    }
                    _ => panic!("unexpected request"),
                }
                BaselineFrame::new(
                    request.transaction_id,
                    request.message_type - 0x20,
                    2,
                    0,
                    payload,
                )
            }))
        }
    }

    fn setup() -> (Upload, Arc<AtomicU8>, Arc<Mutex<Vec<u16>>>) {
        let sync = Arc::new(AtomicU8::new(7));
        let sequences = Arc::new(Mutex::new(Vec::new()));
        let session = OmciSession::new(Mock {
            pending: None,
            sync: sync.clone(),
            sequences: sequences.clone(),
            delay_type: 0,
            ready_at: Instant::now(),
            sent: Arc::default(),
        });
        let upload = Upload::begin(session, Instant::now() + Duration::from_secs(1)).unwrap();
        (upload, sync, sequences)
    }

    #[test]
    fn resumes_after_budget_expires_and_publishes_only_complete_upload() {
        let (mut upload, _, sequences) = setup();
        assert!(!upload.session.next_mib_record(&mut upload.upload).unwrap());
        assert!(!upload.session.next_mib_record(&mut upload.upload).unwrap());
        assert!(!upload
            .step(Instant::now() - Duration::from_millis(1))
            .unwrap());
        assert_eq!(*sequences.lock().unwrap(), vec![0]);
        assert!(upload
            .step(Instant::now() + Duration::from_secs(1))
            .unwrap());
        upload
            .verify(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let report = upload.finish();
        assert!(report.uploaded_complete);
        assert_eq!(*sequences.lock().unwrap(), vec![0, 1]);
    }

    #[test]
    fn rejects_snapshot_if_mib_data_sync_changed() {
        let (mut upload, sync, _) = setup();
        assert!(upload
            .step(Instant::now() + Duration::from_secs(1))
            .unwrap());
        sync.store(8, Ordering::SeqCst);
        assert_eq!(
            upload
                .verify(Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn in_flight_start_and_next_survive_poll_slice_without_new_requests() {
        for delay_type in [0x4d, 0x4e] {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let session = OmciSession::new(Mock {
                pending: None,
                sync: Arc::new(AtomicU8::new(7)),
                sequences: Arc::default(),
                delay_type,
                ready_at: Instant::now(),
                sent: sent.clone(),
            });
            let mut upload =
                Upload::begin(session, Instant::now() + Duration::from_secs(1)).unwrap();
            assert!(!upload
                .step(Instant::now() + Duration::from_millis(45))
                .unwrap());
            std::thread::sleep(Duration::from_millis(90));
            assert!(upload
                .step(Instant::now() + Duration::from_secs(1))
                .unwrap());
            upload
                .verify(Instant::now() + Duration::from_secs(1))
                .unwrap();
            assert!(upload.finish().uploaded_complete);
            let sent = sent.lock().unwrap();
            assert_eq!(sent.iter().filter(|f| f.message_type == 0x4d).count(), 1);
            let sequences: Vec<_> = sent
                .iter()
                .filter(|f| f.message_type == 0x4e)
                .map(|f| u16::from_be_bytes([f.payload[0], f.payload[1]]))
                .collect();
            assert_eq!(sequences, vec![0, 1]);
        }
    }

    #[test]
    fn cancellation_and_registration_change_invalidate_a_paused_upload() {
        use std::sync::atomic::AtomicBool;
        for registration_change in [false, true] {
            let (mut upload, _, _) = setup();
            assert!(!upload.session.next_mib_record(&mut upload.upload).unwrap());
            let token = Arc::new(AtomicBool::new(false));
            upload.session.cancel_when(token.clone());
            if registration_change {
                upload.session.guard_registration(|| false);
            } else {
                token.store(true, Ordering::Release);
            }
            assert_eq!(
                upload
                    .step(Instant::now() + Duration::from_secs(1))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotConnected
            );
        }
    }

    #[test]
    fn expired_snapshot_cannot_resume_or_pass_final_verification() {
        let (mut upload, _, _) = setup();
        upload.started = Instant::now() - Duration::from_secs(601);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            upload.step(deadline).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            upload.verify(deadline).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
