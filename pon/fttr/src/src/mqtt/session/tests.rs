use super::*;
use crate::mqtt::test_peer as peer;
use rumqttc::{SubAck, SubscribeReasonCode};
use std::io::Read;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_rejected_or_mismatched_suback_publishes_zero() {
    for mismatch in [false, true] {
        let (address, thread) = peer::spawn(move |mut stream| {
            let Packet::Subscribe(sub) = peer::read(&mut stream).unwrap() else {
                panic!()
            };
            peer::write(
                &mut stream,
                Packet::SubAck(SubAck::new(
                    sub.pkid + u16::from(mismatch),
                    vec![if mismatch {
                        SubscribeReasonCode::Success(QoS::AtMostOnce)
                    } else {
                        SubscribeReasonCode::Failure
                    }],
                )),
                true,
            );
            assert!(
                peer::read(&mut stream).is_err(),
                "rejected subscription published"
            );
        });
        let mut session = CommandSession::connect(address, "session-reject", deadline())
            .await
            .unwrap();
        assert!(session.subscribe("/reply", deadline()).await.is_err());
        assert!(session
            .enqueue_publish("/cmd", b"harmless fixture")
            .is_err());
        assert!(session.next(deadline()).await.is_err());
        thread.join().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_enqueue_is_not_flushed_and_no_replay_after_deadline() {
    let (address, thread) = peer::spawn(|mut stream| {
        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "enqueue alone sent bytes"
        );
    });
    let mut session = CommandSession::connect(address, "session-queue", deadline())
        .await
        .unwrap();
    session.enqueue_publish("/cmd", b"fixture").unwrap();
    assert!(session.flushed(Instant::now()).await.is_err());
    assert!(session.next(deadline()).await.is_err());
    thread.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_flushed_publication_is_observed_once() {
    let (address, thread) = peer::spawn(|mut stream| {
        let Packet::Publish(publish) = peer::read(&mut stream).unwrap() else {
            panic!()
        };
        assert_eq!(publish.topic, "/cmd");
        assert_eq!(&publish.payload[..], b"fixture");
        assert!(peer::read(&mut stream).is_err());
    });
    let mut session = CommandSession::connect(address, "session-write", deadline())
        .await
        .unwrap();
    session.enqueue_publish("/cmd", b"fixture").unwrap();
    session.flushed(deadline()).await.unwrap();
    drop(session);
    thread.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_socket_flush_stall_is_terminal_and_cancellation_closes_socket() {
    let (address, thread) = peer::spawn(|mut stream| {
        std::thread::sleep(Duration::from_millis(300));
        let mut data = Vec::new();
        stream.read_to_end(&mut data).unwrap();
        assert!(data.len() < 3_145_728, "entire frame unexpectedly flushed");
    });
    let mut session = CommandSession::connect(address, "session-stall", deadline())
        .await
        .unwrap();
    // The peer does not drain: a max frame exceeds this Linux socket's buffer.
    session.enqueue_publish("x", &vec![0; 3_145_720]).unwrap();
    assert!(session
        .flushed(Instant::now() + Duration::from_millis(50))
        .await
        .is_err());
    assert!(session.next(deadline()).await.is_err());
    drop(session);
    thread.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_drop_cancels_without_detached_polling() {
    let (address, thread) = peer::spawn(|mut stream| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let mut session = CommandSession::connect(address, "session-cancel", deadline())
        .await
        .unwrap();
    session.enqueue_publish("/cmd", b"fixture").unwrap();
    drop(session);
    thread.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn mqtt_session_cancelled_poll_cannot_be_resumed() {
    let (address, thread) = peer::spawn(|mut stream| {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let mut session = CommandSession::connect(address, "session-poll-cancel", deadline())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), session.next(deadline()))
            .await
            .is_err()
    );
    assert!(session.next(deadline()).await.is_err());
    thread.join().unwrap();
}

#[test]
fn mqtt_session_exact_total_frame_and_library_header_rejection() {
    let packet = rumqttc::Publish::new("x", QoS::AtMostOnce, vec![0; 3_145_720]);
    assert_eq!(packet.size(), 3_145_728);
    assert_eq!(
        crate::mqtt_limits::publish_size(1, packet.payload.len()).unwrap(),
        packet.size()
    );
    assert!(crate::mqtt_limits::publish_size(1, packet.payload.len() + 1).is_err());
    assert!(crate::mqtt_limits::publish_size(65_536, 0).is_err());
    let mut bytes = bytes::BytesMut::new();
    Packet::Publish(packet)
        .write(&mut bytes, 3_145_728)
        .unwrap();
    assert_eq!(
        rumqttd::protocol::v4::check(bytes.iter(), MAX_REMAINING_BYTES)
            .unwrap()
            .frame_length(),
        3_145_728
    );
    assert!(Packet::read(&mut bytes, MAX_REMAINING_BYTES).is_ok());
    let header = [0x30, 0xfc, 0xff, 0xbf, 1];
    assert!(matches!(
        rumqttc::mqttbytes::check(header.iter(), MAX_REMAINING_BYTES),
        Err(rumqttc::mqttbytes::Error::PayloadSizeLimitExceeded(_))
    ));
    assert!(
        rumqttc::mqttbytes::check([0x30, 0xff, 0xff, 0xff, 0x80].iter(), MAX_REMAINING_BYTES)
            .is_err()
    );
    assert!(rumqttd::protocol::v4::check(header.iter(), MAX_REMAINING_BYTES).is_err());
}
