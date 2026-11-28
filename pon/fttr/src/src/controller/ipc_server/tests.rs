use super::*;
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};

#[cfg(feature = "platform-h3c-hm2004-du")]
mod requests;

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

struct Server {
    path: PathBuf,
    accept: Option<thread::JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
}

impl Server {
    fn new(count: usize, handler: impl Fn(UnixStream, &str) + Send + Sync + 'static) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fttr-ipc-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let accept = thread::spawn(move || {
            serve(
                listener
                    .incoming()
                    .take_while(move |_| !stop.load(Ordering::Acquire))
                    .take(count),
                handler,
            )
        });
        Self {
            path,
            accept: Some(accept),
            stopped,
        }
    }

    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(&self.path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
    }

    fn request(&self, line: &[u8]) -> String {
        let mut stream = self.connect();
        stream.write_all(line).unwrap();
        reply(stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        drop(UnixStream::connect(&self.path));
        self.accept.take().unwrap().join().unwrap();
        std::fs::remove_file(&self.path).unwrap();
    }
}

fn reply(mut stream: UnixStream) -> String {
    let mut response = String::new();
    match stream.read_to_string(&mut response) {
        Ok(_) => {}
        Err(error)
            if error.kind() == io::ErrorKind::ConnectionReset && response.as_bytes() == BUSY => {}
        Err(error) => panic!("socket reply failed: {error}"),
    }
    response
}

fn signal(receiver: &Receiver<()>) {
    receiver.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn status_completes_when_another_client_stalls_during_intake() {
    let server = Server::new(2, |mut stream, line| {
        stream.write_all(line.as_bytes()).unwrap()
    });
    let mut stalled = server.connect();
    stalled.write_all(b"ACS ").unwrap();

    let response = server.request(b"STATUS\n");

    assert_eq!(response, "STATUS");
    drop(stalled);
}

#[test]
fn status_completes_when_all_handlers_wait_for_nested_status() {
    let (entered, waiting) = channel();
    let (release, released) = channel();
    let released = Mutex::new(released);
    let path = Arc::new(std::sync::OnceLock::<PathBuf>::new());
    let nested_path = Arc::clone(&path);
    let server = Server::new(HANDLER_WORKERS + 2, move |mut stream, line| {
        if line.starts_with("MIB ") {
            entered.send(()).unwrap();
            released.lock().unwrap().recv().unwrap();
        }
        if line == "MIB nested" {
            let mut nested = UnixStream::connect(nested_path.get().unwrap()).unwrap();
            nested
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            nested.write_all(b"STATUS\n").unwrap();
            stream.write_all(reply(nested).as_bytes()).unwrap();
        } else {
            stream.write_all(line.as_bytes()).unwrap();
        }
    });
    path.set(server.path.clone()).unwrap();
    let mut slow = Vec::new();
    for index in 0..HANDLER_WORKERS {
        let mut stream = server.connect();
        stream
            .write_all(if index == 0 {
                b"MIB nested\n"
            } else {
                b"MIB {}\n"
            })
            .unwrap();
        slow.push(stream);
        signal(&waiting);
    }

    let response = server.request(b"STATUS\n");

    assert_eq!(response, "STATUS");
    for _ in 0..HANDLER_WORKERS {
        release.send(()).unwrap();
    }
    for (index, stream) in slow.into_iter().enumerate() {
        assert_eq!(reply(stream), if index == 0 { "STATUS" } else { "MIB {}" });
    }
}

#[test]
fn another_handler_completes_when_one_handler_is_slow() {
    let (entered, waiting) = channel();
    let (release, released) = channel();
    let released = Mutex::new(released);
    let server = Server::new(2, move |mut stream, line| {
        if line == "ACS slow" {
            entered.send(()).unwrap();
            released.lock().unwrap().recv().unwrap();
        }
        stream.write_all(line.as_bytes()).unwrap();
    });
    let mut slow = server.connect();
    slow.write_all(b"ACS slow\n").unwrap();
    signal(&waiting);

    let response = server.request(b"PROVISIONING fast\n");

    assert_eq!(response, "PROVISIONING fast");
    release.send(()).unwrap();
    assert_eq!(reply(slow), "ACS slow");
}

#[test]
fn busy_is_returned_when_handler_queue_is_saturated() {
    let (entered, waiting) = channel();
    let (release, released) = channel();
    let released = Mutex::new(released);
    let server = Server::new(HANDLER_WORKERS * 2 + 1, move |mut stream, line| {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
        stream.write_all(line.as_bytes()).unwrap();
    });
    let mut slow = Vec::new();
    for _ in 0..HANDLER_WORKERS {
        let mut stream = server.connect();
        stream.write_all(b"ACS slow\n").unwrap();
        slow.push(stream);
        signal(&waiting);
    }
    let (responded, responses) = channel();
    let mut readers = Vec::new();
    for _ in 0..=HANDLER_WORKERS {
        let mut stream = server.connect();
        stream.write_all(b"ACS queued\n").unwrap();
        let responded = responded.clone();
        readers.push(thread::spawn(move || {
            responded.send(reply(stream)).unwrap()
        }));
    }

    let busy = responses.recv_timeout(Duration::from_secs(2)).unwrap();

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&busy).unwrap()["error"]["code"],
        "busy"
    );
    for _ in 0..HANDLER_WORKERS * 2 {
        release.send(()).unwrap();
    }
    for stream in slow {
        assert_eq!(reply(stream), "ACS slow");
    }
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn intake_slots_are_reused_when_requests_fail() {
    let server = Server::new(INTAKE_WORKERS + 1, |mut stream, line| {
        stream.write_all(line.as_bytes()).unwrap()
    });
    for _ in 0..INTAKE_WORKERS {
        let mut stream = server.connect();
        stream.write_all(&[255, b'\n']).unwrap();
        assert_eq!(reply(stream), "");
    }

    let response = server.request(b"STATUS\n");

    assert_eq!(response, "STATUS");
}

#[test]
fn status_completes_when_another_client_stalls_response_writes() {
    let (entered, waiting) = channel();
    let (finished, completed) = channel();
    let server = Server::new(2, move |mut stream, line| {
        if line == "ACS large" {
            entered.send(()).unwrap();
            let result = stream.write_all(&vec![b'x'; 8 * 1024 * 1024]);
            finished.send(result.is_err()).unwrap();
        } else {
            stream.write_all(line.as_bytes()).unwrap();
        }
    });
    let mut slow = server.connect();
    slow.write_all(b"ACS large\n").unwrap();
    signal(&waiting);

    let response = server.request(b"STATUS\n");

    assert_eq!(response, "STATUS");
    assert!(matches!(
        completed.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    drop(slow);
    assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
}
