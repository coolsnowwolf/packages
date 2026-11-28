//! Telemetry has its own clean-session reconnect policy; command sessions never retry.
use crate::{
    ipc::{self, SubDevice},
    mqtt::session::CommandSession,
};
use rumqttc::{Event, Packet};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

/// Poll and ingest on one owner. There is no intermediate payload channel, clone
/// queue or per-message task. The existing parser worker owns the only pending
/// payload budget (8 MiB); decoder and currently parsing frames are additional.
pub async fn run(address: SocketAddr, devices: Arc<Mutex<Vec<SubDevice>>>) -> Result<(), String> {
    loop {
        let id = format!(
            "fttr-telemetry-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        match receive(address, &id, &devices).await {
            Err(error) => eprintln!("[-] MQTT telemetry disconnected: {error}; reconnecting in 2s"),
            Ok(()) => return Err("MQTT telemetry receive loop unexpectedly returned".into()),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn receive(
    address: SocketAddr,
    id: &str,
    devices: &Mutex<Vec<SubDevice>>,
) -> Result<(), String> {
    let mut session =
        CommandSession::connect(address, id, Instant::now() + Duration::from_secs(5)).await?;
    session
        .subscribe(
            "/fttr/maindev/data/#",
            Instant::now() + Duration::from_secs(5),
        )
        .await?;
    loop {
        // Keep-alive events refresh this idle deadline. A failed poll drops this
        // session; only telemetry constructs a new one after the bounded delay.
        if let Event::Incoming(Packet::Publish(packet)) = session
            .next(Instant::now() + Duration::from_secs(60))
            .await?
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            ingest(&packet.topic, &packet.payload, now, devices)?;
        }
    }
}

fn ingest(
    topic: &str,
    payload: &[u8],
    now: u64,
    devices: &Mutex<Vec<SubDevice>>,
) -> Result<(), String> {
    let Some(source) = topic.strip_prefix("/fttr/maindev/data/") else {
        return Ok(());
    };
    let Some(mac) = crate::mqtt_identity::normalize_target(source) else {
        return Ok(());
    };
    let protected = crate::mqtt_identity::shared()
        .lock()
        .map_err(|_| "MQTT identity lock poisoned")?
        .snapshots(now)
        .into_iter()
        .filter(|source| source.reason == "ready")
        .map(|source| source.mqtt_mac)
        .collect();
    let mut devs = devices.lock().map_err(|_| "MQTT inventory lock poisoned")?;
    crate::telemetry_sources::observe(
        &mut devs,
        SubDevice {
            id: mac.clone(),
            last_seen: now,
            payload_bytes: payload.len(),
        },
        &protected,
    );
    let _ = ipc::update_status_file(&devs);
    drop(devs);
    // Observation only, in the same order as the original ingestion path.
    crate::provisioning::observe_mqtt(&mac, now);
    if !crate::devices::observe_mqtt_report(&mac, payload, now) {
        eprintln!("[-] Telemetry report not queued for {mac}");
    }
    Ok(())
}
