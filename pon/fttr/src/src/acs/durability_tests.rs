use super::*;
use std::io::BufRead;

pub(super) const DEVICE: &str = "ABCDEF:test:durability";
const NS: &str = "urn:dslforum-org:cwmp-1-0";

fn record(id: u64) -> Task {
    Task {
        id,
        device: DEVICE.into(),
        method: "SetParameterValues".into(),
        args: json!({"parameters":[{"name":"InternetGatewayDevice.LANDevice.1.Test","value":"value"}]}),
        state: TaskState::Queued,
        created: now(),
        sent: None,
        result: None,
    }
}

pub(super) struct Fixture(pub(super) Arc<Server>);

impl Fixture {
    pub(super) fn new() -> Self {
        let mut server = tests::listener_test_server();
        Arc::get_mut(&mut server).unwrap().enabled = true;
        {
            let mut state = server.state.lock().unwrap();
            state.devices.insert(
                DEVICE.into(),
                Device {
                    serial: "durability".into(),
                    oui: "ABCDEF".into(),
                    product_class: "test".into(),
                    peer: "127.0.0.1".into(),
                    namespace: NS.into(),
                    last_inform: now(),
                    endpoint_id: String::new(),
                    inform: json!({}),
                },
            );
            state
                .sessions
                .insert("cookie".into(), (DEVICE.into(), "127.0.0.1".into(), now()));
            server.save(&state).unwrap();
        }
        Self(server)
    }

    pub(super) fn fail_save(&self) {
        fs::create_dir(format!("{}.tmp", self.0.state_file)).unwrap();
    }

    pub(super) fn repair_save(&self) {
        fs::remove_dir(format!("{}.tmp", self.0.state_file)).unwrap();
    }

    pub(super) fn queue(&self) -> Value {
        let task = record(0);
        self.0.execute_small(
            json!({"op":"queue","device":DEVICE,"method":task.method,"args":task.args}),
        )
    }

    pub(super) fn exchange(&self, body: &str) -> (String, String) {
        self.exchange_session(body, "cookie")
    }

    pub(super) fn exchange_session(&self, body: &str, cookie: &str) -> (String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let server = self.0.clone();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve(&server, stream).unwrap();
        });
        write!(client, "POST /cwmp HTTP/1.1\r\nHost: localhost\r\nCookie: fttr_session={cookie}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        client.flush().unwrap();
        let mut reader = BufReader::new(client);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap_or_else(|| {
                assert!(headers.starts_with("HTTP/1.1 204"));
                "0"
            })
            .parse()
            .unwrap();
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        worker.join().unwrap();
        (headers, String::from_utf8(bytes).unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.0.state_file).unwrap();
    }
}

#[test]
fn unresolved_writes_are_not_evicted_under_history_pressure() {
    let fixture = Fixture::new();
    let mut state = fixture.0.state.lock().unwrap();
    for id in 1..=256 {
        let mut task = record(id);
        task.state = if id % 2 == 0 {
            TaskState::Unknown
        } else {
            TaskState::AcceptedPendingApply
        };
        task.sent = Some(now());
        task.release_request();
        state.tasks.insert(id, task);
    }
    state.next_id = 256;

    let reply = fixture.0.enqueue(&mut state, record(257));

    assert_eq!(reply["code"], "backpressure");
    assert_eq!(state.tasks.len(), 256);
    assert!(state.tasks.values().all(Task::unresolved_write));
}

#[test]
fn clean_and_dispatch_preserve_unresolved_write_while_reads_progress() {
    let fixture = Fixture::new();
    {
        let mut state = fixture.0.state.lock().unwrap();
        let mut unresolved = record(1);
        unresolved.state = TaskState::AcceptedPendingApply;
        unresolved.sent = Some(now());
        unresolved.release_request();
        state.tasks.insert(1, unresolved);
        state.tasks.insert(2, record(2));
        let mut read = record(3);
        read.method = "GetParameterValues".into();
        read.args = json!({"names":["InternetGatewayDevice.DeviceInfo.ModelName"]});
        state.tasks.insert(3, read);
    }
    let cleaned: Value = serde_json::from_str(&fixture.0.execute(json!({"op":"clean"}))).unwrap();
    assert_eq!(cleaned["tasks"], 3);

    let mut state = fixture.0.state.lock().unwrap();
    assert!(state.next(DEVICE, NS).unwrap().is_none());
    assert_eq!(state.tasks[&2].state, TaskState::Fault);
    let read = state.next(DEVICE, NS).unwrap().expect("read can proceed");
    assert!(read.xml.contains("<cwmp:GetParameterValues>"));
    assert!(state.tasks[&1].unresolved_write());
}

#[test]
fn queue_rejects_failed_save_without_losing_history_or_consuming_id() {
    let fixture = Fixture::new();
    {
        let mut state = fixture.0.state.lock().unwrap();
        for id in 1..=256 {
            let mut task = record(id);
            task.state = TaskState::Completed;
            task.result = Some(error_result("retained history"));
            state.tasks.insert(id, task);
        }
        state.next_id = 256;
        fixture.0.save(&state).unwrap();
    }
    let before = fs::read(&fixture.0.state_file).unwrap();
    fixture.fail_save();

    let reply = fixture.queue();

    assert!(reply["error"].is_string(), "{reply}");
    let state = fixture.0.state.lock().unwrap();
    assert_eq!(serde_json::to_vec(&*state).unwrap(), before);
    assert_eq!(fs::read(&fixture.0.state_file).unwrap(), before);
    drop(state);
    fixture.repair_save();
    assert_eq!(fixture.queue()["id"], 257);
}

#[test]
fn reset_rejects_failed_save_without_losing_tasks_results_or_sessions() {
    let fixture = Fixture::new();
    assert_eq!(fixture.queue()["id"], 1);
    {
        let mut state = fixture.0.state.lock().unwrap();
        let task = state.tasks.get_mut(&1).unwrap();
        task.state = TaskState::Completed;
        task.result = Some(error_result("retained result"));
        fixture.0.save(&state).unwrap();
    }
    let before = fs::read(&fixture.0.state_file).unwrap();
    fixture.fail_save();

    let reply = fixture
        .0
        .execute_small(json!({"op":"reset_device_tasks","device":DEVICE}));

    assert!(reply["error"].is_string());
    let state = fixture.0.state.lock().unwrap();
    assert_eq!(serde_json::to_vec(&*state).unwrap(), before);
    assert!(state.sessions.contains_key("cookie"));
    assert_eq!(fs::read(&fixture.0.state_file).unwrap(), before);
    drop(state);
    fixture.repair_save();
    assert_eq!(
        fixture
            .0
            .execute_small(json!({"op":"reset_device_tasks","device":DEVICE}))["reset"],
        true
    );
    let state = fixture.0.state.lock().unwrap();
    assert!(state.tasks.is_empty());
    assert!(!state.sessions.contains_key("cookie"));
    assert!(State::load(&fixture.0.state_file).unwrap().tasks.is_empty());
}

#[test]
fn dispatch_sends_no_soap_when_sent_save_fails_and_can_retry_after_repair() {
    let fixture = Fixture::new();
    assert_eq!(fixture.queue()["id"], 1);
    fixture.fail_save();

    let (headers, body) = fixture.exchange("");

    assert!(headers.starts_with("HTTP/1.1 503"), "{headers}");
    assert!(body.is_empty());
    let state = fixture.0.state.lock().unwrap();
    assert_eq!(state.tasks[&1].state, TaskState::Queued);
    assert_eq!(state.tasks[&1].sent, None);
    assert!(state.intents.is_empty());
    drop(state);
    assert_eq!(
        State::load(&fixture.0.state_file).unwrap().tasks[&1].state,
        TaskState::Queued
    );
    fixture.repair_save();
    let (headers, body) = fixture.exchange("");
    assert!(headers.starts_with("HTTP/1.1 200"));
    assert_eq!(soap::parse(&body).unwrap().id, "fttr-1");
    let state = fixture.0.state.lock().unwrap();
    let intent = state
        .intents
        .get(&1)
        .expect("sent write has original intent");
    assert!(intent.matches_method("SetParameterValues"));
    assert!(body.contains(&format!(
        "<ParameterKey>{}</ParameterKey>",
        intent.parameter_key
    )));
    let checkpoint = super::persistence::restart_state(&state);
    assert_eq!(checkpoint.intents.get(&1), Some(intent));
    assert!(!serde_json::to_string(&checkpoint)
        .unwrap()
        .contains("\"value\""));
}

#[test]
fn response_plus_next_save_failure_never_requeues_the_already_emitted_write() {
    let fixture = Fixture::new();
    assert_eq!(fixture.queue()["id"], 1);
    assert_eq!(fixture.queue()["id"], 2);
    let (_, first) = fixture.exchange("");
    assert_eq!(soap::parse(&first).unwrap().id, "fttr-1");
    fixture.fail_save();
    let response = soap::envelope(
        NS,
        "fttr-1",
        "<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>",
    );

    let (headers, body) = fixture.exchange(&response);

    assert!(headers.starts_with("HTTP/1.1 503"));
    assert!(body.is_empty());
    let state = fixture.0.state.lock().unwrap();
    assert_eq!(state.tasks[&1].state, TaskState::Completed);
    let result: Value =
        serde_json::to_value(state.tasks[&1].result.as_ref().unwrap().get()).unwrap();
    assert_eq!(result["status"], "0");
    assert_eq!(state.tasks[&2].state, TaskState::Queued);
    assert!(state.intents.is_empty());
    drop(state);
    let mut restarted = State::load(&fixture.0.state_file).unwrap();
    assert_eq!(restarted.tasks[&1].state, TaskState::Unknown);
    assert!(restarted.next(DEVICE, NS).unwrap().is_none());
    assert_eq!(restarted.tasks[&2].state, TaskState::Fault);
    fixture.repair_save();
    let (_, next) = fixture.exchange("");
    assert_eq!(soap::parse(&next).unwrap().id, "fttr-2");
}
