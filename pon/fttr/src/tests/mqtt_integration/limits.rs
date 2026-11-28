use super::*;
use openwrt_fttr::mqtt::session::{self, CommandSession};
use rumqttc::{Event, Packet, Publish, QoS};
use std::io::Write;

#[test]
fn mqtt_wire_exact_total_frame_and_oversized_declaration() {
    let addrs = addresses();
    let (_fixture, rx) = child(&addrs);
    assert!(ready(&rx));
    session::blocking(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut subscriber =
            CommandSession::connect(addrs[0], "limit-subscriber", deadline).await?;
        subscriber.subscribe("x", deadline).await?;
        let mut publisher = CommandSession::connect(addrs[1], "limit-publisher", deadline).await?;
        let payload: Vec<u8> = (0..3_145_720).map(|n| (n % 251) as u8).collect();
        let send = async {
            publisher.enqueue_publish("x", &payload)?;
            publisher.flushed(deadline).await
        };
        let receive = async {
            loop {
                if let Event::Incoming(Packet::Publish(message)) = subscriber.next(deadline).await?
                {
                    assert_eq!(message.topic, "x");
                    assert_eq!(&message.payload[..], &payload);
                    return Ok::<_, String>(());
                }
            }
        };
        let (sent, received) = tokio::join!(send, receive);
        sent?;
        received?;
        Ok(())
    })
    .unwrap();
    // Bypass the application's outgoing check with the pinned serializer to
    // exercise the broker's remaining-length decoder at total MAX + 1.
    let mut socket = TcpStream::connect(addrs[0]).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = bytes::BytesMut::new();
    Packet::Connect(rumqttc::Connect::new("oversize-peer"))
        .write(&mut bytes, usize::MAX)
        .unwrap();
    socket.write_all(&bytes).unwrap();
    let mut ack = [0; 4];
    std::io::Read::read_exact(&mut socket, &mut ack).unwrap();
    assert_eq!(ack, [0x20, 2, 0, 0]);
    bytes.clear();
    Packet::Publish(Publish::new("x", QoS::AtMostOnce, vec![0; 3_145_721]))
        .write(&mut bytes, usize::MAX)
        .unwrap();
    assert_eq!(bytes.len(), 3_145_729);
    // Only a fragmented fixed header is needed: oversized declarations must
    // close immediately, without waiting for or allocating the body.
    for byte in &bytes[..5] {
        socket.write_all(&[*byte]).unwrap();
    }
    let mut byte = [0];
    match std::io::Read::read(&mut socket, &mut byte) {
        Ok(0) => (),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            ()
        }
        result => panic!("oversized frame connection was not closed: {result:?}"),
    }
}
