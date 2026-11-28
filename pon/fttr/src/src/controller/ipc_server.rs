//! Bounded daemon IPC server.
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

pub(crate) mod metrics;

const INTAKE_WORKERS: usize = 2;
const HANDLER_WORKERS: usize = 2;
const STATUS_WORKERS: usize = 1;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 524296;
const BUSY: &[u8] =
    b"{\"schema_version\":1,\"error\":{\"code\":\"busy\",\"message\":\"IPC workers busy\"}}\n";

struct Connection {
    stream: UnixStream,
    accepted: Instant,
}

struct Request {
    stream: UnixStream,
    line: String,
}

/// Intake is asynchronous; driver/config handlers use bounded blocking slots.
/// STATUS has independent admission so nested status calls cannot deadlock.
struct Dispatch<H> {
    handler: Arc<H>,
    intake: Arc<Semaphore>,
    handlers: Arc<Semaphore>,
    status: Arc<Semaphore>,
    running: Arc<Semaphore>,
    status_running: Arc<Semaphore>,
}

impl<H: Fn(UnixStream, &str) + Send + Sync + 'static> Dispatch<H> {
    fn new(handler: H) -> Self {
        Self {
            handler: Arc::new(handler),
            intake: Arc::new(Semaphore::new(INTAKE_WORKERS * 2)),
            handlers: Arc::new(Semaphore::new(HANDLER_WORKERS * 2)),
            status: Arc::new(Semaphore::new(STATUS_WORKERS * 2)),
            running: Arc::new(Semaphore::new(HANDLER_WORKERS)),
            status_running: Arc::new(Semaphore::new(STATUS_WORKERS)),
        }
    }

    fn accept(&self, stream: UnixStream) {
        metrics::accepted();
        let Ok(intake) = self.intake.clone().try_acquire_owned() else {
            reject(stream);
            return;
        };
        let handler = self.handler.clone();
        let handlers = self.handlers.clone();
        let status = self.status.clone();
        let running = self.running.clone();
        let status_running = self.status_running.clone();
        let accepted = Instant::now();
        crate::runtime::spawn_service("IPC request", async move {
            let request = read_request_async(Connection { stream, accepted }).await;
            drop(intake);
            let request = match request {
                Ok(request) => request,
                Err(error) => {
                    metrics::read_error();
                    eprintln!("[-] IPC request read failed: {error}");
                    return;
                }
            };
            let is_status = request.line.trim() == "STATUS";
            let slots = if is_status { status } else { handlers };
            let Ok(admission) = slots.try_acquire_owned() else {
                reject(request.stream);
                return;
            };
            let running = if is_status { status_running } else { running };
            let Ok(execution) = running.acquire_owned().await else {
                return;
            };
            if let Err(error) = crate::runtime::spawn_blocking(move || {
                let (_admission, _execution) = (admission, execution);
                let started = Instant::now();
                handler(request.stream, request.line.trim());
                metrics::handled(started.elapsed());
            })
            .await
            {
                eprintln!("[-] IPC handler failed: {error}");
            }
        });
    }
}

pub(crate) fn start(
    listener: UnixListener,
    handler: impl Fn(UnixStream, &str) + Send + Sync + 'static,
) {
    if let Err(error) = listener.set_nonblocking(true) {
        eprintln!("[-] IPC listener setup failed: {error}");
        return;
    }
    crate::runtime::spawn_service("IPC listener", async move {
        let listener = match tokio::net::UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("[-] IPC listener setup failed: {error}");
                return;
            }
        };
        let dispatch = Dispatch::new(handler);
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = crate::runtime::cancelled() => break,
            };
            match accepted {
                Ok((stream, _)) => match stream.into_std() {
                    Ok(stream) => dispatch.accept(stream),
                    Err(error) => eprintln!("[-] IPC socket setup failed: {error}"),
                },
                Err(error) => {
                    eprintln!("[-] IPC accept failed: {error}");
                    break;
                }
            }
        }
    });
}

async fn read_request_async(connection: Connection) -> io::Result<Request> {
    let Connection { stream, accepted } = connection;
    stream.set_nonblocking(true)?;
    let mut stream = tokio::net::UnixStream::from_std(stream)?;
    let deadline = tokio::time::Instant::from_std(accepted + SOCKET_TIMEOUT);
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        if Instant::now() >= accepted + SOCKET_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IPC request deadline exceeded",
            ));
        }
        let available = buffer.len().min(MAX_REQUEST_BYTES - bytes.len());
        let count = tokio::time::timeout_at(deadline, stream.read(&mut buffer[..available]))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "IPC request deadline exceeded")
            })??;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete IPC request",
            ));
        }
        let received = &buffer[..count];
        if let Some(end) = received.iter().position(|byte| *byte == b'\n') {
            bytes.extend_from_slice(&received[..=end]);
            let line = String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let stream = stream.into_std()?;
            stream.set_nonblocking(false)?;
            stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
            metrics::intake(accepted.elapsed());
            return Ok(Request { stream, line });
        }
        bytes.extend_from_slice(received);
        if bytes.len() == MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IPC request exceeds byte limit",
            ));
        }
    }
}

fn reject(mut stream: UnixStream) {
    metrics::rejected();
    if let Err(error) = stream
        .set_nonblocking(true)
        .and_then(|()| stream.write_all(BUSY))
    {
        eprintln!("[-] IPC busy response failed: {error}");
    }
}

#[cfg(test)]
use std::io::Read;
#[cfg(test)]
use std::{
    sync::{mpsc, Mutex},
    thread,
};

#[cfg(test)]
fn serve(
    streams: impl Iterator<Item = io::Result<UnixStream>>,
    handler: impl Fn(UnixStream, &str) + Send + Sync + 'static,
) {
    let dispatch = Dispatch::new(handler);
    for stream in streams {
        dispatch.accept(stream.unwrap());
    }
}

#[cfg(test)]
fn read_request(connection: Connection) -> io::Result<Request> {
    crate::runtime::block_on(read_request_async(connection))
}

#[cfg(test)]
mod tests;
