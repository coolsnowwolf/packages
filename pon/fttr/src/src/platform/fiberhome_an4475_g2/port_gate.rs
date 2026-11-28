//! Independently expires VPORT liveness while the OMCI owner is blocked.
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Switch = dyn Fn(bool) -> io::Result<()> + Send + Sync;

struct State {
    open: bool,
    deadline: Option<Instant>,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    switch: Box<Switch>,
    cancellation: Arc<AtomicBool>,
}

pub(super) struct PortGate {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    timeout: Duration,
}

impl PortGate {
    pub fn new(
        timeout: Duration,
        cancellation: Arc<AtomicBool>,
        switch: impl Fn(bool) -> io::Result<()> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        // Fail closed before publishing the gate or starting its worker.
        switch(false)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                open: false,
                deadline: None,
                stop: false,
            }),
            wake: Condvar::new(),
            switch: Box::new(switch),
            cancellation,
        });
        let monitor = shared.clone();
        let worker = thread::Builder::new()
            .name("fsl-port-watch".into())
            .spawn(move || {
                let mut state = monitor.state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let expired = state.deadline.is_some_and(|d| Instant::now() >= d);
                    if state.open
                        && (state.stop || expired || monitor.cancellation.load(Ordering::Acquire))
                    {
                        match (monitor.switch)(false) {
                            Ok(()) => {
                                state.open = false;
                                state.deadline = None;
                            }
                            Err(error) => eprintln!("VPORT watchdog close failed: {error}"),
                        }
                    }
                    // Retain ownership until the kernel transition is confirmed.
                    // Otherwise a transient final close failure leaves a live port
                    // behind and lets a replacement worker race its cleanup.
                    if state.stop && !state.open {
                        break;
                    }
                    let wait = state
                        .deadline
                        .map(|d| d.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::from_secs(1))
                        .min(Duration::from_secs(1));
                    // On close failure retry at a bounded rate instead of spinning.
                    let wait = if wait.is_zero() {
                        Duration::from_secs(1)
                    } else {
                        wait
                    };
                    state = monitor
                        .wake
                        .wait_timeout(state, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            timeout,
        })
    }

    /// A response timestamp is captured by the caller before entering the lock.
    pub fn responsive(&self, at: Instant) -> io::Result<bool> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| io::Error::other("VPORT gate lock poisoned"))?;
        let deadline = at + self.timeout;
        if state.stop
            || self.shared.cancellation.load(Ordering::Acquire)
            || Instant::now() >= deadline
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "VPORT liveness response expired",
            ));
        }
        let opened = !state.open;
        if opened {
            // An error may follow a successful kernel transition. Treat the
            // outcome as open until a subsequent close succeeds.
            state.open = true;
            state.deadline = Some(Instant::now());
            if let Err(error) = (self.shared.switch)(true) {
                self.shared.wake.notify_one();
                return Err(error);
            }
        }
        state.deadline = Some(deadline);
        self.shared.wake.notify_one();
        Ok(opened)
    }

    pub fn close(&self) -> io::Result<()> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| io::Error::other("VPORT gate lock poisoned"))?;
        if state.open {
            // Mark expired first so the monitor retries even if this fails.
            state.deadline = Some(Instant::now());
            self.shared.wake.notify_one();
            (self.shared.switch)(false)?;
            state.open = false;
            state.deadline = None;
        }
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.shared.state.lock().is_ok_and(|state| state.open)
    }
}

impl Drop for PortGate {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stop = true;
        self.shared.wake.notify_one();
        drop(state);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn monitor_closes_while_owner_is_blocked_and_reopens_only_on_response() {
        let (tx, rx) = mpsc::channel();
        let gate = PortGate::new(
            Duration::from_millis(60),
            Arc::new(AtomicBool::new(false)),
            move |up| {
                tx.send(up).unwrap();
                Ok(())
            },
        )
        .unwrap();
        assert!(!rx.recv().unwrap());
        assert!(gate.responsive(Instant::now()).unwrap());
        assert!(rx.recv().unwrap());
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!gate.is_open());
        assert!(gate.responsive(Instant::now()).unwrap());
        assert!(rx.recv().unwrap());
        drop(gate);
        assert!(!rx.recv().unwrap());
    }

    #[test]
    fn expired_response_does_not_open() {
        let gate = PortGate::new(
            Duration::from_millis(50),
            Arc::new(AtomicBool::new(false)),
            |_| Ok(()),
        )
        .unwrap();
        assert!(gate
            .responsive(Instant::now() - Duration::from_secs(1))
            .is_err());
        assert!(!gate.is_open());
    }

    #[test]
    fn failed_open_is_treated_as_indeterminate_and_closed() {
        let (tx, rx) = mpsc::channel();
        let gate = PortGate::new(
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
            move |up| {
                tx.send(up).unwrap();
                if up {
                    Err(io::Error::other("unknown outcome"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert!(!rx.recv().unwrap());
        assert!(gate.responsive(Instant::now()).is_err());
        assert!(rx.recv().unwrap());
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!gate.is_open());
    }

    #[test]
    fn failed_expiry_close_is_retried() {
        use std::sync::atomic::AtomicUsize;
        let (tx, rx) = mpsc::channel();
        let closes = Arc::new(AtomicUsize::new(0));
        let calls = closes.clone();
        let gate = PortGate::new(
            Duration::from_millis(30),
            Arc::new(AtomicBool::new(false)),
            move |up| {
                tx.send(up).unwrap();
                if !up && calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    Err(io::Error::other("transient close failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert!(!rx.recv().unwrap());
        gate.responsive(Instant::now()).unwrap();
        assert!(rx.recv().unwrap());
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!rx.recv_timeout(Duration::from_secs(3)).unwrap());
        assert!(!gate.is_open());
    }

    #[test]
    fn failed_final_close_is_retried_before_drop_returns() {
        use std::sync::atomic::AtomicUsize;
        let closes = Arc::new(AtomicUsize::new(0));
        let calls = closes.clone();
        let gate = PortGate::new(
            Duration::from_secs(10),
            Arc::new(AtomicBool::new(false)),
            move |up| {
                if !up && calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    Err(io::Error::other("transient final close failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        gate.responsive(Instant::now()).unwrap();
        drop(gate);
        assert_eq!(closes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cancellation_closes_gate_and_rejects_new_responses() {
        let (tx, rx) = mpsc::channel();
        let cancellation = Arc::new(AtomicBool::new(false));
        let gate = PortGate::new(Duration::from_secs(10), cancellation.clone(), move |up| {
            tx.send(up).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(!rx.recv().unwrap());
        gate.responsive(Instant::now()).unwrap();
        assert!(rx.recv().unwrap());
        cancellation.store(true, Ordering::Release);
        assert!(gate.responsive(Instant::now()).is_err());
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!gate.is_open());
    }
}
