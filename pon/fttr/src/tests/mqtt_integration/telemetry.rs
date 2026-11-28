use super::*;
use openwrt_fttr::{
    controller::mqtt,
    devices::hl202::telemetry::store,
    mqtt::session::{self, CommandSession},
};
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

pub(super) fn report() -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    // Incompressible-enough synthetic data makes this a multi-kilobyte report.
    let raw = (0..12000u32)
        .map(|n| char::from(b' ' + (n.wrapping_mul(2654435761) >> 25) as u8 % 90))
        .collect::<String>();
    for (name, bytes) in [
        ("IP", b"192.0.2.10".as_slice()),
        ("unknown", raw.as_bytes()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes).unwrap();
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    gzip.finish().unwrap()
}

async fn publish(address: SocketAddr, topic: &str, payload: &[u8]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut peer = CommandSession::connect(address, "telemetry-fixture-publisher", deadline)
        .await
        .unwrap();
    peer.enqueue_publish(topic, payload).unwrap();
    peer.flushed(deadline).await.unwrap();
}

async fn deliver_until(address: SocketAddr, payload: &[u8], accepted: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while !accepted() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "telemetry did not arrive"
        );
        publish(
            address,
            "/fttr/maindev/data/02:aa:bb:cc:dd:ee/fttr_data.tar.gz",
            payload,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn mqtt_telemetry_real_delivery_validation_and_reconnect() {
    let addrs = addresses();
    let (fixture, rx) = child(&addrs);
    assert!(ready(&rx));
    let devices = Arc::new(Mutex::new(Vec::new()));
    let payload = report();
    assert!(payload.len() > 2048);
    session::blocking(async {
        let task = tokio::spawn(mqtt::run(addrs[0], devices.clone()));
        deliver_until(addrs[1], &payload, || {
            store::snapshot("02AABBCCDDEE")
                .and_then(|s| s.report)
                .is_some_and(|r| r.fields.get("ip").is_some_and(|f| f.value == "192.0.2.10"))
        })
        .await;
        {
            let devs = devices.lock().unwrap();
            assert_eq!(devs.len(), 1);
            assert_eq!(devs[0].id, "02AABBCCDDEE");
            assert_eq!(devs[0].payload_bytes, payload.len());
        }
        let status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(openwrt_fttr::ipc::STATUS_FILE).unwrap())
                .unwrap();
        assert_eq!(status["devices"][0]["id"], "02AABBCCDDEE");
        assert_eq!(status["devices"][0]["last_payload_bytes"], payload.len());
        // The public inventory observes transport receipt without asserting execution.
        let list = openwrt_fttr::devices::hl202::api::control("{\"method\":\"list\"}", &[]);
        assert!(list["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["mqtt_mac"] == "02AABBCCDDEE"));
        println!("telemetry inventory: {list}");
        publish(addrs[1], "/fttr/maindev/data/../../bad", b"bad").await;
        assert_eq!(devices.lock().unwrap().len(), 1);
        drop(fixture);
        let (restarted, rx) = child(&addrs);
        assert!(ready(&rx));
        // Different receipt length proves delivery after the new clean subscription.
        deliver_until(addrs[1], b"malformed archive", || {
            store::snapshot("02AABBCCDDEE").is_some_and(|s| s.error.is_some())
        })
        .await;
        assert_eq!(devices.lock().unwrap()[0].payload_bytes, 17);
        // A bad archive retains the last valid report, and ingestion keeps working.
        assert_eq!(
            store::snapshot("02AABBCCDDEE")
                .unwrap()
                .report
                .unwrap()
                .fields["ip"]
                .value,
            "192.0.2.10"
        );
        deliver_until(addrs[1], &payload, || {
            store::snapshot("02AABBCCDDEE")
                .is_some_and(|s| s.error.is_none() && s.last_good.is_some())
        })
        .await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(restarted);
        Ok(())
    })
    .unwrap();
}
