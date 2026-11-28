use super::{http, Server};
use std::net::TcpListener;
use std::sync::{atomic::Ordering, Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

// Shared by current and retained endpoints: migrations do not multiply capacity.
fn capacity() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(128))).clone()
}

pub(super) fn start(server: Arc<Server>, listener: TcpListener) {
    println!("[ACS] listening on {}{}", server.listen, server.path);
    crate::runtime::spawn_service("ACS listener", async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                *server.failure.lock().unwrap() = Some(format!("listener setup: {error}"));
                server.stopped.store(true, Ordering::Release);
                return;
            }
        };
        let slots = capacity();
        let mut connections = JoinSet::new();
        while !server.stopping.load(Ordering::Acquire) && !crate::runtime::is_stopping() {
            tokio::select! {
                result = listener.accept() => match result {
                    Ok((stream, _)) => {
                        let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                        let server = server.clone();
                        connections.spawn(async move {
                            let _slot = slot;
                            if let Err(error) = http::serve(server, stream).await {
                                eprintln!("[ACS] HTTP connection ended: {}", error.kind());
                            }
                        });
                    }
                    Err(error) => {
                        *server.failure.lock().unwrap() = Some(format!("listener accept: {error}"));
                        break;
                    }
                },
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result { eprintln!("[ACS] connection task failed: {error}"); }
                },
                _ = tokio::time::sleep(Duration::from_millis(20)) => {},
            }
        }
        drop(listener);
        server.stopped.store(true, Ordering::Release);
        // Existing sessions may complete, but cannot hold a retired endpoint forever.
        let _ = tokio::time::timeout(Duration::from_secs(30), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
    });
}
