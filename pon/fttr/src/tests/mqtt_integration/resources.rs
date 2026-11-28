//! Ignored subprocess workload driven by the resource evidence recorder.
//! It never invokes a shell or contacts a gateway.
use super::*;
use openwrt_fttr::mqtt::session::{self, CommandSession};
use rumqttc::{AsyncClient, Event, Outgoing, Packet, QoS};

fn stage(name: &str) {
    use std::io::Write;
    println!("RESOURCE_STAGE {name}");
    std::io::stdout().flush().unwrap();
    let mut proceed = String::new();
    assert!(std::io::stdin().read_line(&mut proceed).unwrap() > 0);
}

async fn batch(addresses: &[SocketAddr], topic: &str, payload: &[u8], count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut subscriber = CommandSession::connect(addresses[0], "resource-subscriber", deadline)
        .await
        .unwrap();
    subscriber.subscribe(topic, deadline).await.unwrap();
    let (client, mut events) =
        AsyncClient::new(session::options(addresses[1], "resource-publisher"), 1);
    assert!(matches!(
        events.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    let sender = async {
        for _ in 0..count {
            client
                .try_publish(topic, QoS::AtMostOnce, false, payload)
                .unwrap();
            loop {
                if matches!(
                    events.poll().await.unwrap(),
                    Event::Outgoing(Outgoing::Publish(0))
                ) {
                    break;
                }
            }
        }
    };
    let receiver = async {
        for _ in 0..count {
            loop {
                if let Event::Incoming(Packet::Publish(packet)) =
                    subscriber.next(deadline).await.unwrap()
                {
                    assert_eq!(packet.topic, topic);
                    assert_eq!(&packet.payload[..], payload);
                    break;
                }
            }
        }
    };
    tokio::time::timeout_at(deadline, async {
        tokio::join!(sender, receiver);
    })
    .await
    .unwrap();
}

#[test]
#[ignore = "resource workload helper, driven and reaped by evidence recorder"]
fn mqtt_resource_workload_process() {
    let addresses = std::env::var("FTTR_TEST_LISTEN")
        .unwrap()
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect::<Vec<SocketAddr>>();
    session::blocking(async {
        let topic = "/fttr/maindev/data/02AABBCCDDEE/fttr_data.tar.gz";
        batch(&addresses, topic, &super::telemetry::report(), 200).await;
        stage("steady_valid_reports_200");
        // Exact maximum TOTAL frame, not a purported valid compressed archive.
        let maximum = vec![0x5a; 3 * 1024 * 1024 - 5 - 2 - topic.len()];
        batch(&addresses, topic, &maximum, 16).await;
        stage("maximum_frames_16");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut slow =
            CommandSession::connect(addresses[0], "resource-slow-consumer", deadline).await?;
        slow.subscribe("/fttr/pressure/#", deadline).await?;
        let (client, mut events) =
            AsyncClient::new(session::options(addresses[1], "resource-pressure"), 1);
        events.poll().await.map_err(|e| e.to_string())?;
        let pressure = vec![0x7b; 256 * 1024];
        for _ in 0..128 {
            client
                .try_publish(
                    "/fttr/pressure/fixed",
                    QoS::AtMostOnce,
                    false,
                    pressure.as_slice(),
                )
                .unwrap();
            loop {
                if matches!(
                    events.poll().await.unwrap(),
                    Event::Outgoing(Outgoing::Publish(0))
                ) {
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        stage("slow_unpolled_subscriber_32MiB_sent");
        drop(slow);
        for n in 0..64 {
            let peer =
                CommandSession::connect(addresses[0], &format!("resource-reconnect-{n}"), deadline)
                    .await?;
            drop(peer);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stage("reconnects_64");
        for n in 0..512 {
            client
                .try_publish(
                    format!("/fttr/pressure/unique/{n}"),
                    QoS::AtMostOnce,
                    false,
                    vec![0x33; 1024],
                )
                .unwrap();
            loop {
                if matches!(
                    events.poll().await.unwrap(),
                    Event::Outgoing(Outgoing::Publish(0))
                ) {
                    break;
                }
            }
        }
        stage("unique_topics_512");
        drop(client);
        drop(events);
        tokio::time::sleep(Duration::from_secs(5)).await;
        stage("post_drain_5s");
        Ok(())
    })
    .unwrap();
}
