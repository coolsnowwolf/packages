use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn hyper_handles_chunked_expect_continue_and_rejects_ambiguous_framing() {
    let server = transition(
        tests::listener_test_server(),
        true,
        "127.0.0.1:0",
        "/cwmp",
        "",
        "",
        false,
    )
    .unwrap();
    crate::runtime::block_on(async {
        let body = soap::envelope("urn:dslforum-org:cwmp-1-0", "inform", "<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>framing</ProductClass><SerialNumber>1</SerialNumber></DeviceId></cwmp:Inform>");
        let mut stream = tokio::net::TcpStream::connect(&server.listen)
            .await
            .unwrap();
        stream.write_all(b"POST /cwmp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n").await.unwrap();
        let mut interim = [0u8; 25];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut interim))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        stream
            .write_all(format!("{:x}\r\n{}\r\n0\r\n\r\n", body.len(), body).as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("InformResponse"));

        let mut stream = tokio::net::TcpStream::connect(&server.listen)
            .await
            .unwrap();
        stream.write_all(b"POST /cwmp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\nx").await.unwrap();
        response.clear();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert_eq!(server.state.lock().unwrap().devices.len(), 1);
    });
    stop_listener(&server);
    fs::remove_file(&server.state_file).unwrap();
}

async fn post(address: &str, serial: usize) -> String {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let body = soap::envelope("urn:dslforum-org:cwmp-1-0", "inform", &format!(
        "<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>load</ProductClass><SerialNumber>{serial}</SerialNumber></DeviceId></cwmp:Inform>"
    ));
    stream.write_all(format!("POST /cwmp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut reply))
        .await
        .unwrap()
        .unwrap();
    reply
}

#[test]
fn sixty_four_cpes_progress_while_other_connections_stall_in_headers() {
    let server = transition(
        tests::listener_test_server(),
        true,
        "127.0.0.1:0",
        "/cwmp",
        "",
        "",
        false,
    )
    .unwrap();
    crate::runtime::block_on(async {
        let mut stalled = Vec::new();
        for _ in 0..8 {
            let mut stream = tokio::net::TcpStream::connect(&server.listen)
                .await
                .unwrap();
            stream
                .write_all(b"POST /cwmp HTTP/1.1\r\nHost:")
                .await
                .unwrap();
            stalled.push(stream);
        }
        let mut clients = tokio::task::JoinSet::new();
        for serial in 0..64 {
            let address = server.listen.clone();
            clients.spawn(async move { post(&address, serial).await });
        }
        while let Some(reply) = clients.join_next().await {
            let reply = reply.unwrap();
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
            assert!(reply.contains("InformResponse"));
        }
        assert_eq!(server.state.lock().unwrap().devices.len(), 64);
        assert!(post(&server.listen, 65).await.starts_with("HTTP/1.1 503"));
        assert!(post(&server.listen, 0).await.starts_with("HTTP/1.1 200"));
        drop(stalled);
    });
    stop_listener(&server);
    fs::remove_file(&server.state_file).unwrap();
}
