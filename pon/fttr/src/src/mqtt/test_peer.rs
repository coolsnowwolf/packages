//! Fault peers use the pinned library codec, never the removed client/codec.
use bytes::BytesMut;
use rumqttc::{ConnAck, ConnectReturnCode, Packet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

pub fn spawn(
    action: impl FnOnce(TcpStream) + Send + 'static,
) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(matches!(read(&mut stream).unwrap(), Packet::Connect(_)));
        write(
            &mut stream,
            Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)),
            false,
        );
        action(stream);
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err(), "command reconnected");
    });
    (address, handle)
}

pub fn read(stream: &mut TcpStream) -> Result<Packet, String> {
    let mut bytes = BytesMut::new();
    loop {
        match Packet::read(&mut bytes, 3_145_723) {
            Ok(packet) => return Ok(packet),
            Err(rumqttc::mqttbytes::Error::InsufficientBytes(_)) => (),
            Err(error) => return Err(error.to_string()),
        }
        // Single-byte reads keep the simple fault peer from losing coalesced
        // packets. This is fixture I/O, not a retained transport codec.
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&byte);
    }
}

pub fn write(stream: &mut TcpStream, packet: Packet, fragmented: bool) {
    let mut bytes = BytesMut::new();
    packet.write(&mut bytes, 3_145_728).unwrap();
    if fragmented {
        for chunk in bytes.chunks(1) {
            stream.write_all(chunk).unwrap();
        }
    } else {
        stream.write_all(&bytes).unwrap();
    }
}
