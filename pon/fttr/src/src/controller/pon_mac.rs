//! Desired bits are reconciled only by the PLOAM hardware owner.
use std::sync::{Mutex, OnceLock};

/// Shared owner-side completion policy; platform adapters retain register layout.
pub(crate) fn reconcile(
    desired: (bool, bool),
    quiesce: impl FnOnce() -> std::io::Result<()>,
    write: impl FnOnce(bool, bool) -> std::io::Result<crate::platform::resources::PonMacReadback>,
) -> std::io::Result<()> {
    let result = apply(desired, quiesce, write).and_then(|value| {
        if value.tx_enabled != Some(desired.0) || value.fec_enabled != Some(desired.1) {
            Err(std::io::Error::other("GPON TX/FEC readback mismatch"))
        } else {
            Ok(value)
        }
    });
    let error = result.as_ref().err().map(ToString::to_string);
    record(result);
    error.map_or(Ok(()), |error| Err(std::io::Error::other(error)))
}

/// Common cadence for external configuration updates and hardware readback.
pub(crate) struct Poll {
    last: std::time::Instant,
}
impl Default for Poll {
    fn default() -> Self {
        Self {
            last: std::time::Instant::now() - std::time::Duration::from_secs(2),
        }
    }
}
impl Poll {
    pub(crate) fn due(&mut self) -> bool {
        if self.last.elapsed() < std::time::Duration::from_secs(1) {
            return false;
        }
        self.last = std::time::Instant::now();
        if let Err(error) = crate::optical::refresh() {
            eprintln!("[-] Optical configuration: {error}");
        }
        true
    }
}

pub(crate) fn apply<T>(
    desired: (bool, bool),
    quiesce: impl FnOnce() -> std::io::Result<()>,
    write: impl FnOnce(bool, bool) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let cleanup = if !desired.0 { quiesce() } else { Ok(()) };
    // Failure to drain workers must not prevent disabling transmission.
    let written = write(desired.0, desired.1);
    match (cleanup, written) {
        (Ok(()), result) => result,
        (Err(error), Ok(_)) => Err(error),
        (Err(cleanup), Err(write)) => Err(std::io::Error::other(format!(
            "cleanup: {cleanup}; GPON control: {write}"
        ))),
    }
}

fn cache() -> &'static Mutex<Option<fttr_api::PonMacStatus>> {
    static CACHE: OnceLock<Mutex<Option<fttr_api::PonMacStatus>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn desired() -> (bool, bool) {
    let settings = crate::optical::settings();
    (
        settings.enabled && settings.downstream_tx_enabled,
        settings.enabled && settings.fec_enabled,
    )
}

pub(crate) fn record(result: std::io::Result<crate::platform::resources::PonMacReadback>) -> bool {
    let desired = desired();
    let (readback, error) = match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let allowed = readback
        .as_ref()
        .is_some_and(|value| value.tx_enabled == Some(true));
    *cache().lock().unwrap() = Some(fttr_api::PonMacStatus {
        desired_tx_enabled: desired.0,
        desired_fec_enabled: desired.1,
        register: readback.as_ref().and_then(|value| value.raw),
        tx_enabled: readback.as_ref().and_then(|value| value.tx_enabled),
        fec_enabled: readback.as_ref().and_then(|value| value.fec_enabled),
        error,
    });
    allowed
}

pub(crate) fn snapshot() -> fttr_api::PonMacStatus {
    let desired = desired();
    let mut status = cache()
        .lock()
        .unwrap()
        .clone()
        .unwrap_or(fttr_api::PonMacStatus {
            desired_tx_enabled: desired.0,
            desired_fec_enabled: desired.1,
            register: None,
            tx_enabled: None,
            fec_enabled: None,
            error: Some("awaiting_hardware_owner".into()),
        });
    status.desired_tx_enabled = desired.0;
    status.desired_fec_enabled = desired.1;
    status
}

#[cfg(test)]
mod tests {
    use super::apply;
    use std::cell::RefCell;

    #[test]
    fn disable_invalidates_workers_before_write_even_when_cleanup_fails() {
        for failed in [false, true] {
            let events = RefCell::new(Vec::new());
            let result = apply(
                (false, true),
                || {
                    events.borrow_mut().push("quiesce");
                    if failed {
                        Err(std::io::Error::other("carrier"))
                    } else {
                        Ok(())
                    }
                },
                |tx, fec| {
                    assert!(!tx);
                    assert!(fec);
                    events.borrow_mut().push("write");
                    Ok(8)
                },
            );
            assert_eq!(*events.borrow(), ["quiesce", "write"]);
            assert_eq!(result.is_err(), failed);
        }
    }

    #[test]
    fn fec_changes_with_transmission_enabled_do_not_drop_registrations() {
        for fec in [false, true] {
            assert!(apply(
                (true, fec),
                || panic!("must not quiesce active transmission"),
                |tx, actual_fec| {
                    assert!(tx);
                    assert_eq!(actual_fec, fec);
                    Ok(if fec { 10 } else { 2 })
                }
            )
            .is_ok());
        }
    }

    #[test]
    fn completion_rejects_unknown_or_mismatched_readback() {
        for (tx, fec) in [
            (None, Some(true)),
            (Some(false), Some(true)),
            (Some(true), None),
            (Some(true), Some(false)),
        ] {
            assert!(super::reconcile(
                (true, true),
                || panic!("active TX must not quiesce"),
                |_, _| Ok(crate::platform::resources::PonMacReadback {
                    raw: None,
                    tx_enabled: tx,
                    fec_enabled: fec
                })
            )
            .is_err());
        }
    }
}
