use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct ChildGuard(Child, std::path::PathBuf);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_file(&self.1);
    }
}

fn child(mode: &str) -> (ChildGuard, mpsc::Receiver<String>) {
    let path =
        std::env::temp_dir().join(format!("fttr-lifetime-{}-{mode}.sock", std::process::id()));
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "controller::lifetime::tests::controller_mqtt_lifetime_child",
            "--nocapture",
        ])
        .env("FTTR_LIFETIME_MODE", mode)
        .env("FTTR_LIFETIME_SOCKET", &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    (ChildGuard(child, path), rx)
}

fn ready(rx: &mpsc::Receiver<String>) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("no lifecycle ready");
        if line == "LIFETIME_READY" {
            return;
        }
    }
}

#[test]
#[ignore = "reaped hardware-independent controller lifetime fixture"]
fn controller_mqtt_lifetime_child() {
    #[cfg(feature = "mqtt")]
    let mode = std::env::var("FTTR_LIFETIME_MODE").unwrap();
    #[cfg(feature = "mqtt")]
    let lifetime = {
        // Fixture-only listener addresses; production uses the netifd LAN address.
        let addresses = ["127.0.0.1:0", "127.0.0.2:0"].map(|a| {
            std::net::TcpListener::bind(a)
                .unwrap()
                .local_addr()
                .unwrap()
        });
        let mut broker =
            crate::runtime::block_on(crate::broker::EmbeddedBroker::start(&addresses)).unwrap();
        if mode == "failed" {
            broker.inject_finished_thread();
        }
        Lifetime {
            broker,
            address: addresses[0],
            lan: None,
        }
    };
    #[cfg(not(feature = "mqtt"))]
    let lifetime = crate::runtime::block_on(Lifetime::start()).unwrap();
    let path = std::env::var("FTTR_LIFETIME_SOCKET").unwrap();
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for request in listener.incoming() {
            let mut request = request.unwrap();
            request.write_all(b"{\"status\":\"ok\"}").unwrap();
        }
    });
    println!("LIFETIME_READY");
    if let Err(error) = crate::runtime::block_on(lifetime.wait(Arc::new(Mutex::new(Vec::new())))) {
        eprintln!("lifecycle fatal: {error}");
        std::process::exit(1);
    }
}

#[test]
fn controller_mqtt_lifetime_serves_status_and_stays_alive() {
    #[cfg(not(feature = "mqtt"))]
    let _port = crate::MQTT_TEST_PORT.lock().unwrap();
    let (mut child, rx) = child("healthy");
    ready(&rx);
    #[cfg(not(feature = "mqtt"))]
    let _no_mqtt_socket = std::net::TcpListener::bind("127.0.0.1:1883").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut queries = 0;
    while Instant::now() < deadline {
        assert!(child.0.try_wait().unwrap().is_none());
        let mut socket = UnixStream::connect(&child.1).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reply = String::new();
        socket.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "{\"status\":\"ok\"}");
        queries += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "mqtt_enabled={}, status_queries={queries}, observation_seconds=5",
        cfg!(feature = "mqtt")
    );
}

#[cfg(feature = "mqtt")]
#[test]
fn controller_mqtt_lifetime_runtime_broker_failure_is_fatal() {
    let (mut child, rx) = child("failed");
    ready(&rx);
    let deadline = Instant::now() + Duration::from_secs(13);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "failed broker left a live daemon"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
