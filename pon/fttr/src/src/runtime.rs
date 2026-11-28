//! One application executor, also used by synchronous device/CLI adapters.
//! Blocking work must be admitted by its service before entering the pool.
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use tokio::runtime::{Builder, Runtime};

fn executor() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(32)
            .thread_name("fttr-async")
            .enable_all()
            .build()
            .expect("create application Tokio runtime")
    })
}

pub(crate) fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    executor().spawn(future)
}

fn services() -> &'static Mutex<Vec<tokio::task::JoinHandle<()>>> {
    static SERVICES: OnceLock<Mutex<Vec<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    SERVICES.get_or_init(|| Mutex::new(Vec::new()))
}

fn stop_signal() -> &'static tokio::sync::watch::Sender<bool> {
    static STOP: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
    STOP.get_or_init(|| tokio::sync::watch::channel(false).0)
}

pub(crate) fn is_stopping() -> bool {
    *stop_signal().borrow()
}

pub(crate) fn request_stop() {
    stop_signal().send_replace(true);
}

pub(crate) async fn cancelled() {
    let mut stop = stop_signal().subscribe();
    let _ = stop.wait_for(|stopping| *stopping).await;
}

pub(crate) fn spawn_service(name: &'static str, future: impl Future<Output = ()> + Send + 'static) {
    let mut services = services().lock().unwrap();
    if is_stopping() {
        return;
    }
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let service = spawn(future);
    let abort = AbortOnDrop(service.abort_handle());
    let task = spawn(async move {
        let _abort = abort;
        if let Err(error) = service.await {
            eprintln!("[-] {name} service failed: {error}");
        }
    });
    services.retain(|task| !task.is_finished());
    services.push(task);
}

/// Stop admission first, then give in-progress bounded transactions time to finish.
pub(crate) async fn shutdown() {
    request_stop();
    let mut tasks = std::mem::take(&mut *services().lock().unwrap());
    let drained = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        for task in &mut tasks {
            if let Err(error) = task.await {
                eprintln!("[-] Service join failed: {error}");
            }
        }
    })
    .await;
    if drained.is_err() {
        for task in tasks {
            task.abort();
        }
        eprintln!("[-] Service shutdown deadline exceeded");
    }
}

pub(crate) fn spawn_blocking<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    executor().spawn_blocking(work)
}

/// Synchronous API/driver boundary. Async services await their futures directly.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| executor().block_on(future))
    } else {
        executor().block_on(future)
    }
}

pub(crate) async fn termination() -> std::io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = term.recv() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // Process isolation matters: shutdown is deliberately irreversible for fttrd.
    #[test]
    fn services_drain_and_stop_admission() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::tests::shutdown_child", "--ignored"])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore = "isolated shutdown fixture; invoked by services_drain_and_stop_admission"]
    fn shutdown_child() {
        block_on(async {
            let drained = Arc::new(AtomicBool::new(false));
            let completed = drained.clone();
            let (ready, started) = tokio::sync::oneshot::channel();
            spawn_service("shutdown fixture", async move {
                ready.send(()).unwrap();
                cancelled().await;
                completed.store(true, Ordering::SeqCst);
            });
            started.await.unwrap();
            request_stop();
            let rejected = Arc::new(AtomicBool::new(false));
            let ran = rejected.clone();
            spawn_service("rejected fixture", async move {
                ran.store(true, Ordering::SeqCst);
            });
            tokio::time::timeout(std::time::Duration::from_secs(4), shutdown())
                .await
                .expect("shutdown must be bounded");
            assert!(drained.load(Ordering::SeqCst));
            assert!(!rejected.load(Ordering::SeqCst));
            assert!(services().lock().unwrap().is_empty());
        });
    }
}
