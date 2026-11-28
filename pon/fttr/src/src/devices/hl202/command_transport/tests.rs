use super::*;
use crate::mqtt::test_peer::{self, read, write};
use rumqttc::{Publish, QoS, SubAck, SubscribeReasonCode};
use std::io::Read;

fn request(timeout: Duration) -> Request<'static> {
    Request {
        target: "1A2B3C4DA111".into(),
        script: "printf '猫';\n$(id); `id`",
        id: "current".into(),
        timeout,
    }
}

pub(crate) fn subscribe(stream: &mut std::net::TcpStream, accepted: bool) {
    let Packet::Subscribe(subscription) = read(stream).unwrap() else {
        panic!("expected subscription")
    };
    assert_eq!(
        subscription.filters[0].path,
        "/fttr/subdev/exec/1A2B3C4DA111/resp"
    );
    let code = if accepted {
        SubscribeReasonCode::Success(QoS::AtMostOnce)
    } else {
        SubscribeReasonCode::Failure
    };
    write(
        stream,
        Packet::SubAck(SubAck::new(subscription.pkid, vec![code])),
        true,
    );
}

fn publication(stream: &mut std::net::TcpStream) {
    let Packet::Publish(published) = read(stream).unwrap() else {
        panic!("expected publication")
    };
    assert_eq!(published.topic, "/fttr/subdev/exec/1A2B3C4DA111");
    assert!(std::str::from_utf8(&published.payload)
        .unwrap()
        .contains("$(id); `id`"));
    assert_eq!(published.qos, QoS::AtMostOnce);
    assert!(!published.retain);
}

fn reply(stream: &mut std::net::TcpStream, topic: &str, id: &str, retained: bool) {
    let mut reply = Publish::new(
        topic,
        QoS::AtMostOnce,
        format!("__FTTR_BEGIN_{id}__\n猫\n__FTTR_END_{id}__=7\n"),
    );
    reply.retain = retained;
    write(stream, Packet::Publish(reply), true);
}

#[test]
fn mqtt_command_transport_ignores_retained_stale_and_wrong_topic_replies() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        subscribe(&mut stream, true);
        publication(&mut stream);
        let topic = "/fttr/subdev/exec/1A2B3C4DA111/resp";
        reply(&mut stream, topic, "current", true);
        reply(&mut stream, topic, "old", false);
        reply(&mut stream, "/different/resp", "current", false);
        reply(&mut stream, topic, "current", false);
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let result = blocking(run(address, request(Duration::from_secs(1)), enqueue)).unwrap();
    assert_eq!(result.output.as_deref(), Some("猫"));
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.success());
    worker.join().unwrap();
}

#[test]
fn mqtt_command_transport_timeout_does_not_republish() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        subscribe(&mut stream, true);
        publication(&mut stream);
        assert_eq!(stream.read(&mut [0]).unwrap(), 0); // Exactly one publication, then closed.
    });
    let result = blocking(run(address, request(Duration::from_millis(250)), enqueue)).unwrap();
    assert_eq!(result.status, CommandStatus::Timeout);
    assert!(!result.success());
    assert!(result.error.unwrap().contains("outcome is unknown"));
    worker.join().unwrap();
}

#[test]
fn suback_wait_consumes_the_same_deadline_and_never_enqueues_after_expiry() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        assert!(matches!(read(&mut stream).unwrap(), Packet::Subscribe(_)));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let started = std::time::Instant::now();
    assert!(blocking(run(
        address,
        request(Duration::from_millis(100)),
        |_, _, _| { panic!("expired subscription must not publish") }
    ))
    .is_err());
    assert!(started.elapsed() < Duration::from_millis(500));
    worker.join().unwrap();
}

#[test]
fn mqtt_command_transport_subscription_rejection_prevents_publication() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        subscribe(&mut stream, false);
        assert_eq!(stream.read(&mut [0]).unwrap(), 0); // No publication before or after rejection.
    });
    assert_eq!(
        blocking(run(address, request(Duration::from_secs(1)), enqueue))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    worker.join().unwrap();
}

#[test]
fn mqtt_command_transport_round_trips_fragmented_tcp_packets() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        subscribe(&mut stream, true);
        publication(&mut stream);
        reply(
            &mut stream,
            "/fttr/subdev/exec/1A2B3C4DA111/resp",
            "current",
            false,
        );
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let result = blocking(run(address, request(Duration::from_secs(2)), enqueue)).unwrap();
    assert_eq!(result.output.as_deref(), Some("猫"));
    assert_eq!(result.exit_code, Some(7));
    worker.join().unwrap();
}

#[test]
fn mqtt_command_disconnect_after_flush_is_unknown_and_never_replayed() {
    let (address, worker) = test_peer::spawn(|mut stream| {
        subscribe(&mut stream, true);
        publication(&mut stream);
    });
    let error = blocking(run(address, request(Duration::from_secs(1)), enqueue)).unwrap_err();
    assert!(error.to_string().contains("unknown"));
    worker.join().unwrap();
}
