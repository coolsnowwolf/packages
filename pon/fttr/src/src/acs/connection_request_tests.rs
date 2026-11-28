use super::connection_request::Worker;
use super::connection_request_http::{request, Outcome, Target};
use super::durability_tests::{Fixture, DEVICE};
use super::*;
use crate::provisioning::model::ConnectionRequest;
use std::io::BufRead;
use std::time::Instant;

fn credentials() -> ConnectionRequest {
    ConnectionRequest {
        enabled: true,
        defaults: false,
        username: "cr-user".into(),
        password: "cr-pass".into(),
        ..Default::default()
    }
}

fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "mock CPE connection timed out");
                std::thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) => panic!("mock CPE accept: {error}"),
        }
    }
}

pub(super) fn mock_cpe(response: String) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/tr69", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let read_headers = |stream: &TcpStream| {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            headers
        };
        let mut stream = accept_with_timeout(&listener);
        assert!(!read_headers(&stream).contains("Authorization:"));
        write!(stream, "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"realm@ecntcwmp\", nonce=\"test-nonce\", qop=\"auth\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        let mut stream = accept_with_timeout(&listener);
        let headers = read_headers(&stream);
        stream.write_all(response.as_bytes()).unwrap();
        headers
    });
    (url, thread)
}

fn fixture(url: &str) -> Fixture {
    let fixture = Fixture::new();
    fixture.0.stopped.store(false, Ordering::Release);
    let mut state = fixture.0.state.lock().unwrap();
    state.devices.get_mut(DEVICE).unwrap().inform = json!({"parameters":[{
        "name":"InternetGatewayDevice.ManagementServer.ConnectionRequestURL", "value":url
    }]});
    fixture.0.save(&state).unwrap();
    drop(state);
    fixture
}

fn configs(url: &str) -> BTreeMap<String, ConnectionRequest> {
    let address = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    BTreeMap::from([(
        DEVICE.into(),
        ConnectionRequest {
            port: address.port().try_into().unwrap(),
            ..credentials()
        },
    )])
}

#[test]
fn targets_reject_destinations_other_than_inform_peer() {
    let invalid = [
        "http://localhost/tr69",
        "http://127.0.0.2/tr69",
        "file:///tmp/a",
        "http://user@127.0.0.1/a",
        "http://127.0.0.1/a#b",
        "http://127.0.0.1/a\nb",
        "http://127.1/a",
        "http://2130706433/a",
        "http://127.0.0.1:0/a",
        "http://127.0.0.1\\@127.0.0.2/a",
        "http://127.0.0.1%00/a",
    ];
    for url in invalid {
        assert!(Target::parse(url, "127.0.0.1").is_err(), "{url}");
    }
    assert!(Target::parse("https://[::1]:7547/tr69?x=1", "::1").is_ok());
}

#[test]
fn digest_acceptance_does_not_deliver_rpc_and_stale_cookie_does_not_suppress_wakeup() {
    let (url, cpe) = mock_cpe(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let fixture = fixture(&url);
    assert!(fixture.exchange("").0.starts_with("HTTP/1.1 204"));
    assert_eq!(fixture.queue()["id"], 1);
    let mut worker = Worker::default();
    let at = Instant::now();

    worker.poll(&fixture.0, &configs(&url), at);

    let auth = cpe.join().unwrap();
    assert!(
        auth.contains("Authorization: Digest username=\"cr-user\""),
        "{auth}"
    );
    assert!(auth.contains("qop=auth"));
    assert!(!auth.contains("cr-pass"));
    assert_eq!(
        fixture.0.execute_small(json!({"op":"devices"}))["connection_requests"][DEVICE]["state"],
        "accepted"
    );
    assert_eq!(
        State::load(&fixture.0.state_file).unwrap().tasks[&1].state,
        TaskState::Queued
    );
    worker.poll(&fixture.0, &configs(&url), at + Duration::from_secs(1));
    assert_eq!(
        fixture.0.execute_small(json!({"op":"devices"}))["connection_requests"][DEVICE]["state"],
        "accepted"
    );
    let inform = soap::envelope("urn:dslforum-org:cwmp-1-0", "event6", &format!("<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>test</ProductClass><SerialNumber>durability</SerialNumber></DeviceId><Event><EventStruct><EventCode>6 CONNECTION REQUEST</EventCode></EventStruct></Event><ParameterList><ParameterValueStruct><Name>InternetGatewayDevice.ManagementServer.ConnectionRequestURL</Name><Value>{}</Value></ParameterValueStruct></ParameterList></cwmp:Inform>", soap::escape(&url)));
    let (headers, body) = fixture.exchange(&inform);
    assert_eq!(soap::parse(&body).unwrap().method, "InformResponse");
    let cookie = headers
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: fttr_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    {
        let mut state = fixture.0.state.lock().unwrap();
        assert_eq!(
            state.devices[DEVICE].inform["events"][0],
            "6 CONNECTION REQUEST"
        );
        assert_eq!(state.tasks[&1].state, TaskState::Queued);
        let session = state.sessions.remove(&cookie).unwrap();
        state.sessions.insert("cookie".into(), session);
    }
    assert_eq!(soap::parse(&fixture.exchange("").1).unwrap().id, "fttr-1");
    let reply = soap::envelope(
        "urn:dslforum-org:cwmp-1-0",
        "fttr-1",
        "<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>",
    );
    assert!(fixture.exchange(&reply).0.starts_with("HTTP/1.1 204"));
    assert_eq!(
        State::load(&fixture.0.state_file).unwrap().tasks[&1].state,
        TaskState::Completed
    );
}

#[test]
fn accepted_request_wakes_next_batch_without_thirty_second_cooldown() {
    let response = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (url, cpe) = mock_cpe(response.into());
    let fixture = fixture(&url);
    fixture.queue();
    let mut worker = Worker::default();
    let at = Instant::now();
    worker.poll(&fixture.0, &configs(&url), at);
    cpe.join().unwrap();
    // An accepted request still awaiting its first session is not sent repeatedly.
    worker.poll(&fixture.0, &configs(&url), at + Duration::from_secs(1));
    assert_eq!(
        fixture.0.execute_small(json!({"op":"devices"}))["connection_requests"][DEVICE]["state"],
        "accepted"
    );
    let (next_url, next_cpe) = mock_cpe(response.into());
    {
        let mut state = fixture.0.state.lock().unwrap();
        state.tasks.get_mut(&1).unwrap().state = TaskState::Completed;
        state.devices.get_mut(DEVICE).unwrap().inform["parameters"][0]["value"] = json!(next_url);
    }
    assert_eq!(fixture.queue()["id"], 2);
    worker.poll(&fixture.0, &configs(&next_url), at + Duration::from_secs(1));
    assert!(next_cpe.join().unwrap().contains("Authorization: Digest"));
    assert_eq!(
        fixture.0.execute_small(json!({"op":"devices"}))["connection_requests"][DEVICE]["state"],
        "accepted"
    );
}

#[test]
fn slow_connection_request_does_not_block_another_device() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let slow_url = format!("http://{}/tr69", listener.local_addr().unwrap());
    let (release, wait) = std::sync::mpsc::channel();
    let slow = std::thread::spawn(move || {
        let _stream = accept_with_timeout(&listener);
        let _ = wait.recv_timeout(Duration::from_secs(6));
    });
    let (fast_url, fast) = mock_cpe(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let fixture = fixture(&slow_url);
    fixture.queue();
    let fast_id = "ZZZZZZ:test:fast";
    {
        let mut state = fixture.0.state.lock().unwrap();
        let mut device = state.devices[DEVICE].clone();
        device.inform["parameters"][0]["value"] = json!(fast_url);
        state.devices.insert(fast_id.into(), device);
        let mut task = state.tasks[&1].clone();
        task.id = 2;
        task.device = fast_id.into();
        state.tasks.insert(2, task);
    }
    let mut config = configs(&slow_url);
    config.insert(fast_id.into(), configs(&fast_url).remove(DEVICE).unwrap());
    crate::runtime::block_on(async {
        let mut worker = Worker::default();
        worker.poll_async(&fixture.0, &config, Instant::now()).await;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let view = fixture.0.execute_small(json!({"op":"devices"}));
            if view["connection_requests"][fast_id]["state"] == "accepted" {
                assert_eq!(view["connection_requests"][DEVICE]["state"], "requesting");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fast device waited behind slow request"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        release.send(()).unwrap();
    });
    slow.join().unwrap();
    fast.join().unwrap();
}

#[test]
fn request_failures_keep_durable_queue_and_restart_uses_latest_inform() {
    let (url, cpe) = mock_cpe(
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let fixture = fixture(&url);
    fixture.queue();
    *fixture.0.state.lock().unwrap() = State::load(&fixture.0.state_file).unwrap();

    Worker::default().poll(&fixture.0, &configs(&url), Instant::now());

    cpe.join().unwrap();
    let view = fixture.0.execute_small(json!({"op":"devices"}));
    assert_eq!(view["connection_requests"][DEVICE]["state"], "rejected");
    assert_eq!(view["connection_requests"][DEVICE]["http_status"], 401);
    assert!(!view.to_string().contains("cr-pass"));
    assert_eq!(
        State::load(&fixture.0.state_file).unwrap().tasks[&1].state,
        TaskState::Queued
    );
}

#[test]
fn disabled_config_and_failed_enqueue_never_attempt_request() {
    let fixture = fixture("http://127.0.0.1:1/tr69");
    fixture.fail_save();
    assert!(fixture.queue()["error"].is_string());
    let mut worker = Worker::default();
    worker.poll(
        &fixture.0,
        &configs("http://127.0.0.1:1/tr69"),
        Instant::now(),
    );
    assert!(fixture
        .0
        .state
        .lock()
        .unwrap()
        .connection_requests
        .is_empty());
    fixture.repair_save();
    fixture.queue();
    let mut config = configs("http://127.0.0.1:1/tr69");
    config.get_mut(DEVICE).unwrap().enabled = false;

    worker.poll(&fixture.0, &config, Instant::now());

    assert!(fixture
        .0
        .state
        .lock()
        .unwrap()
        .connection_requests
        .is_empty());
}

#[test]
fn redirect_and_oversized_body_are_not_request_acceptance() {
    for (reply, expected) in [
        ("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.2/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", Outcome::Rejected { http_status: 302 }),
        ("HTTP/1.1 200 OK\r\nContent-Length: 9000\r\nConnection: close\r\n\r\n", Outcome::ResponseTooLarge),
    ] {
        let (url, cpe) = mock_cpe(reply.into());
        let outcome = request(&Target::parse(&url, "127.0.0.1").unwrap(), &configs(&url)[DEVICE]);
        cpe.join().unwrap();
        assert_eq!(outcome, expected);
    }
}

#[test]
fn credential_config_injection_is_rejected_before_network() {
    let target = Target::parse("http://127.0.0.1:1/tr69", "127.0.0.1").unwrap();
    let mut config = credentials();
    config.password = "secret\nproxy = evil".into();

    let outcome = request(&target, &config);

    assert_eq!(outcome, Outcome::MissingCredentials);
}

#[test]
fn ordinary_queue_cannot_write_either_management_namespace_with_forged_markers() {
    for root in ["InternetGatewayDevice", "Device"] {
        let fixture = fixture("http://127.0.0.1:1/tr69");
        let result = fixture.0.execute_small(json!({"op":"queue", "device":DEVICE,
            "method":"SetParameterValues", "internal_interval":true,
            "args":{"internal_interval":true,"parameters":[{"name":format!("{root}.ManagementServer.PeriodicInformInterval"),"value":"43200","kind":"unsignedInt"}]}}));
        assert!(result["error"].is_string(), "{root}: {result}");
    }
}

#[test]
fn configured_port_is_used_without_changing_inform_peer_or_path() {
    let (url, cpe) = mock_cpe(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let target = Target::parse("http://127.0.0.1:1/tr69", "127.0.0.1").unwrap();

    let outcome = request(&target, &configs(&url)[DEVICE]);

    assert_eq!(outcome, Outcome::Accepted { http_status: 204 });
    assert!(cpe.join().unwrap().starts_with("GET /tr69 HTTP/1.1\r\n"));
}
