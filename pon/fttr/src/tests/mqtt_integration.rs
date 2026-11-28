#![cfg(feature = "mqtt")]
use openwrt_fttr::broker::{lan_addresses, EmbeddedBroker};
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(feature = "device-hl202")]
#[path = "mqtt_integration/commands.rs"]
mod commands;
#[path = "mqtt_integration/limits.rs"]
mod limits;
#[cfg(feature = "device-hl202")]
#[path = "mqtt_integration/resources.rs"]
mod resources;
#[cfg(feature = "device-hl202")]
#[path = "mqtt_integration/telemetry.rs"]
mod telemetry;

// rumqttd has no shutdown API: all broker threads live in a reaped child.
struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn addresses() -> Vec<SocketAddr> {
    ["127.0.0.1", "127.0.0.2"]
        .iter()
        .map(|ip| {
            TcpListener::bind(format!("{ip}:0"))
                .unwrap()
                .local_addr()
                .unwrap()
        })
        .collect()
}
fn child(addrs: &[SocketAddr]) -> (Fixture, mpsc::Receiver<String>) {
    let mut process = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "broker_fixture_process",
            "--nocapture",
        ])
        .env(
            "FTTR_TEST_LISTEN",
            addrs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = process.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (Fixture(process), rx)
}
fn ready(rx: &mpsc::Receiver<String>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(12);
    while let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if line == "FTTR_BROKER_READY" {
            return true;
        }
    }
    false
}

#[test]
#[ignore = "child-process helper, never an ordinary test assertion"]
fn broker_fixture_process() {
    let addrs = std::env::var("FTTR_TEST_LISTEN")
        .unwrap()
        .split(',')
        .map(|a| a.parse().unwrap())
        .collect::<Vec<_>>();
    let health_ms = std::env::var("FTTR_TEST_HEALTH_MS")
        .ok()
        .map(|s| s.parse::<u64>().unwrap())
        .unwrap_or(100);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut broker = EmbeddedBroker::start(&addrs).await.unwrap();
        println!("FTTR_BROKER_READY");
        loop {
            tokio::time::sleep(Duration::from_millis(health_ms)).await;
            broker.check_health().await.unwrap();
        }
    });
}

#[test]
fn broker_lifecycle_cross_listener_and_cleanup() {
    let addrs = addresses();
    let (mut fixture, rx) = child(&addrs);
    assert!(ready(&rx), "no cross-listener ready signal");
    assert!(fixture.0.try_wait().unwrap().is_none());
    for addr in &addrs {
        assert!(TcpStream::connect_timeout(addr, Duration::from_secs(1)).is_ok());
    }
    drop(fixture);
    for addr in &addrs {
        assert!(TcpStream::connect_timeout(addr, Duration::from_secs(1)).is_err());
    }
}

#[test]
fn broker_lifecycle_collision_on_either_listener_is_fatal() {
    for occupied in 0..2 {
        let addrs = addresses();
        // Occupied by a real independently ready broker, not a fake TCP acceptor.
        let (_external, external_ready) = child(&[addrs[occupied]]);
        assert!(ready(&external_ready));
        let (mut fixture, rx) = child(&addrs);
        assert!(
            !ready(&rx),
            "external broker must never satisfy owned readiness"
        );
        assert!(!fixture.0.wait().unwrap().success());
    }
}

#[test]
fn broker_lifecycle_address_validation() {
    for value in [
        "",
        "0.0.0.0",
        "127.0.0.1",
        "::1",
        "192.168.1.1/24",
        "224.0.0.1",
        "255.255.255.255",
    ] {
        assert!(lan_addresses(value).is_err(), "accepted {value}");
    }
    assert_eq!(
        lan_addresses("192.168.1.1").unwrap(),
        [
            "127.0.0.1:1883".parse().unwrap(),
            "192.168.1.1:1883".parse().unwrap()
        ]
    );
    let addrs = ["0.0.0.0:1883".parse().unwrap()];
    let (mut fixture, rx) = child(&addrs);
    assert!(!ready(&rx));
    assert!(!fixture.0.wait().unwrap().success());
}
