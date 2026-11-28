//! Process-owned MQTT 3.1.1 broker. rumqttd has no public shutdown handle:
//! after threads start, any startup/supervision failure must terminate the process.
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rumqttc::{AsyncClient, Event, Packet, QoS, SubscribeReasonCode};

const MAX_REMAINING_BYTES: usize = crate::mqtt_limits::MAX_PACKET_BYTES - 5;

pub struct EmbeddedBroker {
    handle: Option<JoinHandle<Result<(), String>>>,
    addresses: Vec<SocketAddr>,
}

/// Production addresses are fixed to loopback and the current LAN IPv4.
pub fn lan_addresses(lan: &str) -> Result<Vec<SocketAddr>, String> {
    let ip: Ipv4Addr = lan
        .parse()
        .map_err(|_| "MQTT requires a LAN IPv4 address")?;
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() {
        return Err("MQTT requires a non-loopback unicast LAN IPv4 address".into());
    }
    Ok(vec![
        SocketAddr::from((Ipv4Addr::LOCALHOST, 1883)),
        SocketAddr::from((ip, 1883)),
    ])
}

fn configuration(addresses: &[SocketAddr]) -> rumqttd::Config {
    rumqttd::Config {
        router: rumqttd::RouterConfig {
            max_connections: 64,
            max_outgoing_packet_count: 16,
            max_segment_size: 4_194_304,
            max_segment_count: 2,
            // Probes only need a few recent messages, not hours of history.
            custom_segment: Some(HashMap::from([(
                "/fttr/health/#".into(),
                rumqttd::SegmentConfig {
                    max_segment_size: 1024,
                    max_segment_count: 2,
                },
            )])),
            ..Default::default()
        },
        v4: Some(
            addresses
                .iter()
                .enumerate()
                .map(|(i, addr)| {
                    let name = format!("fttr-mqtt-{i}");
                    (
                        name.clone(),
                        rumqttd::ServerSettings {
                            name,
                            listen: *addr,
                            tls: None,
                            next_connection_delay_ms: 1,
                            connections: rumqttd::ConnectionSettings {
                                connection_timeout_ms: 5000,
                                // Pinned v4::check compares REMAINING length, not frame length.
                                max_payload_size: MAX_REMAINING_BYTES,
                                max_inflight_count: 16,
                                auth: None,
                                external_auth: None,
                                dynamic_filters: false,
                            },
                        },
                    )
                })
                .collect::<HashMap<_, _>>(),
        ),
        // v5, WS, TLS, bridge, console, metrics and Prometheus are absent.
        ..Default::default()
    }
}

fn fatal(error: &str) -> ! {
    eprintln!("[-] Owned MQTT broker failed: {error}");
    std::process::exit(1)
}

impl EmbeddedBroker {
    #[cfg(test)]
    pub(crate) fn inject_finished_thread(&mut self) {
        // Used only inside a reaped lifecycle child. Its real broker threads
        // remain process-owned and are reclaimed by the expected fatal exit.
        self.handle = Some(thread::spawn(|| {
            Err("injected runtime broker failure".into())
        }));
    }

    /// Fixture callers may choose isolated ports/loopback aliases. Production
    /// must pass only `lan_addresses`. No caller may treat a failure as fallback.
    pub async fn start(listen_addrs: &[SocketAddr]) -> Result<Self, String> {
        Self::start_with(listen_addrs, |config| {
            let mut broker = rumqttd::Broker::new(config);
            broker.start().map_err(|e| format!("rumqttd start: {e:?}"))
        })
        .await
    }

    async fn start_with(
        listen_addrs: &[SocketAddr],
        run: impl FnOnce(rumqttd::Config) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let mut addresses = listen_addrs.to_vec();
        addresses.sort_unstable();
        addresses.dedup();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|a| !a.is_ipv4() || a.ip().is_unspecified() || a.port() == 0)
        {
            return Err("MQTT requires explicit IPv4 listeners and nonzero ports".into());
        }
        // Hold every preflight bind together, so a conflict starts no threads.
        let reservations = addresses
            .iter()
            .map(|a| {
                TcpListener::bind(a).map_err(|e| format!("MQTT listener {a} unavailable: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = configuration(&addresses);
        drop(reservations);
        let handle = thread::Builder::new()
            .name("fttr-rumqttd".into())
            .spawn(move || run(config))
            .map_err(|e| format!("spawn MQTT broker: {e}"))?;
        let mut broker = Self {
            handle: Some(handle),
            addresses,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            broker.check_thread().unwrap_or_else(|e| fatal(&e));
            let attempt = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .min(Duration::from_millis(500));
            if attempt.is_zero() {
                fatal("startup readiness deadline exceeded");
            }
            match tokio::time::timeout(attempt, cross_listener_probe(&broker.addresses)).await {
                Ok(Ok(())) => break,
                _ if tokio::time::Instant::now() >= deadline => {
                    fatal("startup cross-listener readiness deadline exceeded")
                }
                _ => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        broker.check_thread().unwrap_or_else(|e| fatal(&e));
        Ok(broker)
    }

    fn check_thread(&mut self) -> Result<(), String> {
        match self.handle.as_ref() {
            Some(handle) if !handle.is_finished() => return Ok(()),
            None => return Err("broker thread already terminated".into()),
            _ => (),
        }
        match self.handle.take().unwrap().join() {
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => Err("broker thread unexpectedly returned".into()),
            Err(_) => Err("broker thread panicked".into()),
        }
    }

    /// A fresh exchange checks all listeners share the router. Thread liveness
    /// alone misses an individual listener failure in pinned Broker::start.
    pub async fn check_health(&mut self) -> Result<(), String> {
        self.check_thread()?;
        tokio::time::timeout(
            Duration::from_secs(2),
            cross_listener_probe(&self.addresses),
        )
        .await
        .map_err(|_| "MQTT cross-listener health deadline exceeded".to_string())??;
        self.check_thread()
    }
}

fn probe_client(addr: SocketAddr, id: &str) -> (AsyncClient, rumqttc::EventLoop) {
    AsyncClient::new(crate::mqtt::session::options(addr, id), 1)
}

async fn cross_listener_probe(addresses: &[SocketAddr]) -> Result<(), String> {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    // Health traffic never reaches gateway command/report topics.
    // Reuse the process topic: generating a new topic per check would grow
    // rumqttd's topic registry even after subscribers disconnect.
    let topic = format!("/fttr/health/{}", std::process::id());
    // Checks are serialized by the broker owner. Stable IDs also reuse the
    // disconnected-client metrics; freshness belongs in the payload only.
    let (subscriber, mut receiving) = probe_client(addresses[0], "fttr-health-r");
    match receiving.poll().await.map_err(|e| e.to_string())? {
        Event::Incoming(Packet::ConnAck(_)) => (),
        _ => return Err("health subscriber missing CONNACK".into()),
    }
    subscriber
        .try_subscribe(&topic, QoS::AtMostOnce)
        .map_err(|e| e.to_string())?;
    let mut subscription = None;
    loop {
        match receiving.poll().await.map_err(|e| e.to_string())? {
            Event::Outgoing(rumqttc::Outgoing::Subscribe(id)) => subscription = Some(id),
            Event::Incoming(Packet::SubAck(ack)) => {
                if subscription != Some(ack.pkid)
                    || ack.return_codes != [SubscribeReasonCode::Success(QoS::AtMostOnce)]
                {
                    return Err("health subscription rejected/mismatched".into());
                }
                break;
            }
            _ => (),
        }
    }
    for (i, addr) in addresses.iter().enumerate() {
        let (publisher, mut sending) = probe_client(*addr, &format!("fttr-health-w-{i}"));
        match sending.poll().await.map_err(|e| e.to_string())? {
            Event::Incoming(Packet::ConnAck(_)) => (),
            _ => return Err("health publisher missing CONNACK".into()),
        }
        let payload = format!("{i}:{nonce}");
        publisher
            .try_publish(&topic, QoS::AtMostOnce, false, payload.as_bytes())
            .map_err(|e| e.to_string())?;
        loop {
            if matches!(
                sending.poll().await.map_err(|e| e.to_string())?,
                Event::Outgoing(rumqttc::Outgoing::Publish(_))
            ) {
                break;
            }
        }
        loop {
            if let Event::Incoming(Packet::Publish(report)) =
                receiving.poll().await.map_err(|e| e.to_string())?
            {
                if !report.retain
                    && report.topic == topic
                    && report.payload.as_ref() == payload.as_bytes()
                {
                    break;
                }
                return Err("health nonce mismatch".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "reaped child helper for broker thread failure injection"]
    fn broker_failure_child() {
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let _ = crate::runtime::block_on(EmbeddedBroker::start_with(&[address], |_| {
            Err("injected broker thread failure".into())
        }));
        panic!("failed broker unexpectedly returned ready");
    }

    #[test]
    fn broker_lifecycle_start_thread_failure() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "broker::tests::broker_failure_child",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("injected broker thread failure"));
    }
}
