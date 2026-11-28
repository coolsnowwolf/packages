use super::durability_tests::{Fixture, DEVICE};
use super::*;
use crate::provisioning::model::ConnectionRequest;

const NS: &str = "urn:dslforum-org:cwmp-1-0";

fn config() -> ConnectionRequest {
    ConnectionRequest {
        defaults: false,
        username: "test-user".into(),
        password: "test-password".into(),
        ..Default::default()
    }
}

fn poll(fixture: &Fixture, config: &ConnectionRequest) -> Value {
    periodic_policy::poll(
        &fixture.0,
        &BTreeMap::from([(DEVICE.into(), config.clone())]),
    );
    fixture.0.execute_small(json!({"op":"devices"}))["periodic_inform"][DEVICE].clone()
}

fn dispatch(fixture: &Fixture, expected: &str) -> soap::Message {
    let message = soap::parse(&exchange(fixture, "").1).unwrap();
    assert_eq!(message.method, expected);
    message
}

fn respond(fixture: &Fixture, request: &soap::Message, body: &str) {
    let reply = soap::envelope(NS, &request.id, body);
    assert!(exchange(fixture, &reply).0.starts_with("HTTP/1.1 204"));
}

fn exchange(fixture: &Fixture, body: &str) -> (String, String) {
    let cookie = fixture
        .0
        .state
        .lock()
        .unwrap()
        .sessions
        .keys()
        .next()
        .unwrap()
        .clone();
    fixture.exchange_session(body, &cookie)
}

fn probe(fixture: &Fixture, root: &str) -> ConnectionRequest {
    let (url, cpe) = super::connection_request_tests::mock_cpe(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    );
    let port = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .port();
    let config = ConnectionRequest {
        port: port.try_into().unwrap(),
        ..config()
    };
    fixture
        .0
        .state
        .lock()
        .unwrap()
        .devices
        .get_mut(DEVICE)
        .unwrap()
        .inform = json!({"parameters":[{"name":format!("{root}.ManagementServer.ConnectionRequestURL"),"value":url}]});
    poll(fixture, &config);
    super::connection_request::Worker::default().poll(
        &fixture.0,
        &BTreeMap::from([(DEVICE.into(), config.clone())]),
        std::time::Instant::now(),
    );
    assert!(cpe.join().unwrap().contains("Authorization: Digest"));
    let inform = soap::envelope(NS, "probe", &format!("<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>test</ProductClass><SerialNumber>durability</SerialNumber></DeviceId><Event><EventStruct><EventCode>6 CONNECTION REQUEST</EventCode></EventStruct></Event><ParameterList><ParameterValueStruct><Name>{root}.ManagementServer.ConnectionRequestURL</Name><Value>{}</Value></ParameterValueStruct></ParameterList></cwmp:Inform>",soap::escape(&url)));
    assert_eq!(
        soap::parse(&exchange(fixture, &inform).1).unwrap().method,
        "InformResponse"
    );
    config
}

fn values(fixture: &Fixture, root: &str, enabled: bool, interval: u32) {
    let request = dispatch(fixture, "GetParameterValues");
    respond(fixture, &request, &format!("<cwmp:GetParameterValuesResponse><ParameterList><ParameterValueStruct><Name>{root}.ManagementServer.PeriodicInformEnable</Name><Value>{enabled}</Value></ParameterValueStruct><ParameterValueStruct><Name>{root}.ManagementServer.PeriodicInformInterval</Name><Value>{interval}</Value></ParameterValueStruct></ParameterList></cwmp:GetParameterValuesResponse>"));
}

fn capability(fixture: &Fixture, root: &str, writable: bool) {
    let request = dispatch(fixture, "GetParameterNames");
    respond(fixture, &request, &format!("<cwmp:GetParameterNamesResponse><ParameterList><ParameterInfoStruct><Name>{root}.ManagementServer.PeriodicInformInterval</Name><Writable>{writable}</Writable></ParameterInfoStruct></ParameterList></cwmp:GetParameterNamesResponse>"));
}

fn fixture(root: &str) -> Fixture {
    let fixture = Fixture::new();
    fixture.0.stopped.store(false, Ordering::Release);
    fixture
        .0
        .state
        .lock()
        .unwrap()
        .devices
        .get_mut(DEVICE)
        .unwrap()
        .inform = json!({"parameters":[{"name":format!("{root}.ManagementServer.ConnectionRequestURL"),"value":"http://127.0.0.1:7547/tr69"}]});
    fixture
}

#[test]
fn configured_credentials_without_session_proof_never_queue_a_write() {
    let fixture = fixture("Device");
    let config = config();
    poll(&fixture, &config);
    values(&fixture, "Device", true, 5);
    poll(&fixture, &config);
    if fixture
        .0
        .state
        .lock()
        .unwrap()
        .tasks
        .values()
        .any(|t| t.method == "GetParameterNames")
    {
        capability(&fixture, "Device", true);
        poll(&fixture, &config);
    }
    assert!(fixture
        .0
        .state
        .lock()
        .unwrap()
        .tasks
        .values()
        .all(|t| t.method != "SetParameterValues"));
}

#[test]
fn pending_cycle_consumes_inform_before_idle_polling() {
    let fixture = fixture("Device");
    let config = ConnectionRequest {
        enabled: false,
        ..Default::default()
    };
    poll(&fixture, &config);
    fixture
        .0
        .state
        .lock()
        .unwrap()
        .devices
        .get_mut(DEVICE)
        .unwrap()
        .last_inform += 1;
    values(&fixture, "Device", true, 180);
    poll(&fixture, &config);
    let id = fixture.0.state.lock().unwrap().next_id;
    for _ in 0..3 {
        poll(&fixture, &config);
    }
    assert_eq!(fixture.0.state.lock().unwrap().next_id, id);
}

#[test]
fn interval_policy_uses_real_cwmp_capability_write_and_independent_readback() {
    for root in ["InternetGatewayDevice", "Device"] {
        for status in [0, 1] {
            let fixture = fixture(root);
            let config = probe(&fixture, root);
            poll(&fixture, &config);
            values(&fixture, root, true, 5);
            poll(&fixture, &config);
            capability(&fixture, root, true);
            assert_eq!(poll(&fixture, &config)["state"], "setting");

            let request = dispatch(&fixture, "SetParameterValues");
            let parameters = request.data.rows().unwrap();
            assert_eq!(parameters.len(), 1);
            assert_eq!(
                parameters[0]["name"],
                format!("{root}.ManagementServer.PeriodicInformInterval")
            );
            assert_eq!(parameters[0]["value"], "43200");
            assert_eq!(parameters[0]["type"], "xsd:unsignedInt");
            respond(&fixture, &request, &format!("<cwmp:SetParameterValuesResponse><Status>{status}</Status></cwmp:SetParameterValuesResponse>"));
            assert_ne!(poll(&fixture, &config)["state"], "verified");
            values(&fixture, root, true, 43200);

            let result = poll(&fixture, &config);
            assert_eq!(result["state"], "verified");
            assert!(result["verified_at"].is_u64());
            assert!(!result.to_string().contains("test-password"));
        }
    }
}

#[test]
fn disabled_policy_only_reads_and_requires_manual_enabled_short_interval() {
    for (enabled, interval, expected) in [
        (true, 1, "verified"),
        (true, 180, "verified"),
        (true, 181, "manual_required"),
        (false, 5, "manual_required"),
    ] {
        let fixture = fixture("Device");
        let config = ConnectionRequest {
            enabled: false,
            ..Default::default()
        };
        poll(&fixture, &config);
        values(&fixture, "Device", enabled, interval);

        assert_eq!(poll(&fixture, &config)["state"], expected);
        assert!(fixture
            .0
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .all(|task| task.method == "GetParameterValues"));
    }
}

#[test]
fn unavailable_factory_or_missing_credentials_never_raise_interval() {
    for (config, expected) in [
        (
            ConnectionRequest::default(),
            "default_credentials_unavailable",
        ),
        (
            ConnectionRequest {
                defaults: false,
                ..Default::default()
            },
            "missing_credentials",
        ),
    ] {
        let fixture = fixture("Device");
        poll(&fixture, &config);
        values(&fixture, "Device", true, 5);

        assert_eq!(poll(&fixture, &config)["state"], expected);
        assert!(fixture
            .0
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .all(|task| task.method != "SetParameterValues"));
    }
}

#[test]
fn unsupported_interval_is_not_written_and_readback_mismatch_is_not_verified() {
    for writable in [false, true] {
        let fixture = fixture("Device");
        let config = probe(&fixture, "Device");
        poll(&fixture, &config);
        values(&fixture, "Device", true, 5);
        poll(&fixture, &config);
        capability(&fixture, "Device", writable);
        let result = poll(&fixture, &config);
        if writable {
            let request = dispatch(&fixture, "SetParameterValues");
            respond(&fixture, &request, "<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>");
            poll(&fixture, &config);
            values(&fixture, "Device", true, 5);
            assert_eq!(poll(&fixture, &config)["state"], "unknown");
        } else {
            assert_eq!(result["state"], "unsupported");
            assert!(fixture
                .0
                .state
                .lock()
                .unwrap()
                .tasks
                .values()
                .all(|task| task.method != "SetParameterValues"));
        }
    }
}

#[test]
fn internal_grant_is_exact_revocable_and_not_restored_from_persisted_json() {
    for scenario in [
        "mutate", "revoke", "restart", "target", "endpoint", "config",
    ] {
        let fixture = fixture("Device");
        let config = probe(&fixture, "Device");
        poll(&fixture, &config);
        values(&fixture, "Device", true, 5);
        poll(&fixture, &config);
        capability(&fixture, "Device", true);
        poll(&fixture, &config);
        let mut state = fixture.0.state.lock().unwrap();
        let id = state.next_id;
        match scenario {
            "mutate" => {
                state.tasks.get_mut(&id).unwrap().args["parameters"][0]["name"] =
                    json!("Device.ManagementServer.Password")
            }
            "revoke" => config.authorization.store(false, Ordering::Release),
            "restart" => *state = State::load(&fixture.0.state_file).unwrap(),
            "target" => {
                state.devices.get_mut(DEVICE).unwrap().inform["parameters"][0]["value"] =
                    json!("http://127.0.0.1/different")
            }
            "endpoint" => {
                state.devices.get_mut(DEVICE).unwrap().endpoint_id = "another-endpoint".into()
            }
            "config" => {
                state
                    .connection_request_proofs
                    .get_mut(DEVICE)
                    .unwrap()
                    .config
                    .password = "different".into()
            }
            _ => unreachable!(),
        }

        assert!(
            state.next(DEVICE, NS).unwrap().is_none(),
            "scenario: {scenario}"
        );
        assert_eq!(state.tasks[&id].state, TaskState::Fault);
    }
}

#[test]
fn failed_durable_enqueue_cannot_authorize_interval_write() {
    let fixture = fixture("Device");
    fixture.fail_save();

    poll(&fixture, &config());

    let state = fixture.0.state.lock().unwrap();
    assert!(state.tasks.is_empty());
    assert!(state.interval_authorizations.is_empty());
    drop(state);
    fixture.repair_save();
}
