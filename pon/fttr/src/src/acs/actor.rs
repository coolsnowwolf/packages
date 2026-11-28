//! Bounded, ordered application transactions. Disk writes never
//! occupy an executor worker, and a cancelled HTTP response cannot cancel a
//! transaction halfway through persisting its sent marker.
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};

type Work = Box<dyn FnOnce() + Send>;

struct Mailbox {
    ordinary: mpsc::Sender<Work>,
    responses: mpsc::Sender<Work>,
}
fn sender() -> &'static Mailbox {
    static SENDER: OnceLock<Mailbox> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, mut receiver) = mpsc::channel::<Work>(128);
        let (responses, mut priority) = mpsc::channel::<Work>(16);
        crate::runtime::spawn_service("ACS transactions", async move {
            let mut burst = 0usize;
            loop {
                if crate::runtime::is_stopping() { priority.close(); receiver.close(); }
                let ordinary = (burst >= 4).then(|| receiver.try_recv().ok()).flatten();
                let work = if let Some(work) = ordinary {
                    burst = 0;
                    Some(work)
                } else { tokio::select! {
                    biased;
                    work = priority.recv() => { burst += 1; work.or_else(|| receiver.try_recv().ok()) },
                    work = receiver.recv() => { burst = 0; work.or_else(|| priority.try_recv().ok()) },
                    _ = crate::runtime::cancelled() => { priority.close(); receiver.close(); priority.try_recv().ok().or_else(|| receiver.try_recv().ok()) }
                }};
                let Some(work) = work else { break };
                if let Err(error) = crate::runtime::spawn_blocking(work).await {
                    eprintln!("[ACS] transaction worker failed: {error}");
                }
            }
        });
        Mailbox { ordinary: sender, responses }
    })
}

pub(super) async fn call<F, T>(work: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    call_on(false, work).await
}
pub(super) async fn response<F, T>(work: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    call_on(true, work).await
}
async fn call_on<F, T>(priority: bool, work: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (reply, result) = oneshot::channel();
    let mailbox = sender();
    let sender = if priority {
        &mailbox.responses
    } else {
        &mailbox.ordinary
    };
    sender
        .send(Box::new(move || {
            // Admission is cancellable; once execution starts it is indivisible.
            if !reply.is_closed() {
                let _ = reply.send(work());
            }
        }))
        .await
        .map_err(|_| ())?;
    result.await.map_err(|_| ())
}
