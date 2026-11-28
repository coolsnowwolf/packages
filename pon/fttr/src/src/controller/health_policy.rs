use std::{
    io,
    time::{Duration, Instant},
};

/// Carrier admission for a transparent bridge. A down fttrN port makes the
/// kernel drop every downstream frame, so requiring the first packet to come
/// from the ONU side deadlocks an idle LAN. Raise the port as soon as OMCI is
/// alive; the business state still waits for GEM evidence separately.
pub(crate) fn carrier_ready(last_get: Option<Instant>, now: Instant) -> bool {
    last_get.is_some_and(|time| now.duration_since(time) < Duration::from_secs(15))
}

#[cfg(test)]
pub(crate) fn link_ready(
    last_get: Option<Instant>,
    last_data: Option<Instant>,
    now: Instant,
) -> bool {
    last_get.is_some_and(|time| now.duration_since(time) < Duration::from_secs(15))
        && last_data.is_some()
}

pub(crate) struct Observations {
    watch: bool,
    receive_errors: u64,
    send_errors: u64,
    last_report: Instant,
}

impl Observations {
    pub(crate) fn new(watch: bool) -> Self {
        Self {
            watch,
            receive_errors: 0,
            send_errors: 0,
            last_report: Instant::now(),
        }
    }

    pub(crate) fn receive<T>(&mut self, result: io::Result<Option<T>>) -> io::Result<Option<T>> {
        match result {
            Ok(value) => Ok(value),
            Err(error) if self.watch && resource_pressure(&error) => {
                self.receive_errors = self.receive_errors.saturating_add(1);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn send(&mut self, result: io::Result<()>) -> io::Result<bool> {
        match result {
            Ok(()) => Ok(true),
            Err(error)
                if self.watch
                    && (resource_pressure(&error)
                        || matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        )) =>
            {
                self.send_errors = self.send_errors.saturating_add(1);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn report(&mut self, id: u8, now: Instant) {
        if now.duration_since(self.last_report) >= Duration::from_secs(30)
            && (self.receive_errors != 0 || self.send_errors != 0)
        {
            eprintln!("ONU {id} observer degraded: missed_receives={} unsent_probes={}; no liveness credited", self.receive_errors, self.send_errors);
            self.receive_errors = 0;
            self.send_errors = 0;
            self.last_report = now;
        }
    }
}

fn resource_pressure(error: &io::Error) -> bool {
    // Linux ENOMEM and ENOBUFS: a lost observation, not proof of ONU failure.
    matches!(error.raw_os_error(), Some(12 | 105))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_gem_does_not_expire_current_session_proof() {
        // Given valid GEM evidence from an otherwise idle watch.
        let now = Instant::now();
        let data = now - Duration::from_secs(600);
        // When a fresh OMCI response arrives after ten idle minutes.
        let ready = link_ready(Some(now), Some(data), now);
        // Then idle traffic does not close a verified link.
        assert!(ready);
    }

    #[test]
    fn new_watch_requires_its_own_gem_evidence() {
        let now = Instant::now();
        assert!(!link_ready(Some(now), None, now));
    }

    #[test]
    fn omci_expiry_closes_link_but_recovery_can_reuse_session_gem() {
        let now = Instant::now();
        let old = now - Duration::from_secs(15);
        assert!(!link_ready(Some(old), Some(old), now));
        assert!(link_ready(Some(now), Some(old), now));
        assert!(!link_ready(None, Some(old), now));
    }

    #[test]
    fn transient_capture_errors_are_missing_samples_only_in_watch() {
        for errno in [12, 105] {
            let mut observer = Observations::new(true);
            assert_eq!(
                observer
                    .receive::<u8>(Err(io::Error::from_raw_os_error(errno)))
                    .unwrap(),
                None
            );
            assert_eq!(observer.receive(Ok(Some(7))).unwrap(), Some(7));
            assert_eq!(observer.receive_errors, 1);
            assert!(Observations::new(false)
                .receive::<u8>(Err(io::Error::from_raw_os_error(errno)))
                .is_err());
        }
    }

    #[test]
    fn only_known_transient_probe_errors_are_tolerated() {
        for errno in [4, 11, 12, 105] {
            assert!(!Observations::new(true)
                .send(Err(io::Error::from_raw_os_error(errno)))
                .unwrap());
            assert!(Observations::new(false)
                .send(Err(io::Error::from_raw_os_error(errno)))
                .is_err());
        }
        for errno in [1, 5, 9, 13, 19, 22, 88, 90, 100] {
            assert!(Observations::new(true)
                .send(Err(io::Error::from_raw_os_error(errno)))
                .is_err());
            assert!(Observations::new(true)
                .receive::<u8>(Err(io::Error::from_raw_os_error(errno)))
                .is_err());
        }
        assert!(Observations::new(true).send(Ok(())).unwrap());
    }

    #[test]
    fn successful_samples_do_not_reset_warning_rate_limit() {
        let mut observer = Observations::new(true);
        let start = observer.last_report;
        observer
            .receive::<()>(Err(io::Error::from_raw_os_error(105)))
            .unwrap();
        observer.receive(Ok(Some(()))).unwrap();
        observer.report(1, start + Duration::from_secs(29));
        assert_eq!(observer.receive_errors, 1);
        observer.report(1, start + Duration::from_secs(30));
        assert_eq!(observer.receive_errors, 0);
        assert_eq!(observer.last_report, start + Duration::from_secs(30));
    }
}
