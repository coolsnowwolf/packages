use super::resources::Response;
use super::*;

fn server() -> Arc<Server> {
    super::tests::listener_test_server()
}

fn task(id: u64) -> Task {
    Task {
        id,
        device: "compact-test".into(),
        method: "GetParameterValues".into(),
        args: json!({"names":["InternetGatewayDevice."]}),
        state: TaskState::Sent,
        created: now(),
        sent: Some(now()),
        result: None,
    }
}

#[test]
fn lost_write_response_remains_unresolved_when_result_budget_is_exhausted() {
    for method in [
        "SetParameterValues",
        "AddObject",
        "DeleteObject",
        "GetParameterValues",
    ] {
        let mut state = State::default();
        let held = state
            .budget
            .reserve(resources::RESULT_BYTES, resources::RESULT_BYTES)
            .unwrap();
        let mut pending = task(1);
        pending.method = method.into();
        state.tasks.insert(1, pending);
        let body = format!("<cwmp:{method}Response/>");
        let mut response = soap::parse(&soap::envelope(
            "urn:dslforum-org:cwmp-1-0",
            "fttr-1",
            &body,
        ))
        .unwrap();
        response.data =
            serde_json::from_value(json!({"status":"0", "instance_number":"1"})).unwrap();
        state.response("compact-test", response).unwrap();
        let is_write = method != "GetParameterValues";
        assert_eq!(
            state.tasks[&1].state,
            if is_write {
                TaskState::Unknown
            } else {
                TaskState::Fault
            }
        );
        assert_eq!(state.tasks[&1].unresolved_write(), is_write);
        state.prune();
        assert!(state.tasks.contains_key(&1));
        drop(held);
    }
}

#[test]
fn protected_slow_large_responses_survive_newer_small_results_and_ack() {
    let server = server();
    let mut state = State::default();
    for id in 1..=3 {
        let mut t = task(id);
        t.sent = Some(now() - 60);
        state.tasks.insert(id, t);
        if id < 3 {
            state.results.insert(id, retention::ResultInfo::protected());
        }
        let mut message = soap::parse(&soap::envelope(
            "urn:dslforum-org:cwmp-1-0",
            &format!("fttr-{id}"),
            "<cwmp:GetParameterValuesResponse/>",
        ))
        .unwrap();
        message.data = serde_json::from_value(
            json!({"parameters":[],"padding":"x".repeat(if id < 3 { 753_000 } else { 10 })}),
        )
        .unwrap();
        state.response("compact-test", message).unwrap();
    }
    state.prune();
    for id in 1..=2 {
        assert!(state.tasks[&id].result.as_ref().unwrap().bytes() > 753_000);
        assert!(state.results[&id].completed_at.unwrap() >= now() - 1);
    }
    server.save(&state).unwrap();
    *server.state.lock().unwrap() = State::load(&server.state_file).unwrap();
    // Ordinary task queries, including repeated queries, cannot acknowledge a lease.
    for _ in 0..2 {
        assert!(server.execute_value(json!({"op":"task","id":1}))["result"].is_object());
    }
    let mut state = server.state.lock().unwrap();
    assert!(state.results[&1].protected);
    assert!(state.ack_result(1));
    assert!(!state.ack_result(1));
    assert!(state.tasks[&1].result.is_none());
    state.results.get_mut(&2).unwrap().completed_at = Some(now() - retention::LEASE_SECONDS);
    state.prune();
    assert!(!state.results[&2].protected);
    assert!(state.tasks[&2].result.is_none());
    assert_eq!(state.results[&2].reason, "consumer lease expired");
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn dispatch_waits_without_reserving_queued_results() {
    let mut state = State::default();
    for id in 1..=16 {
        let mut t = task(id);
        t.method = "GetParameterNames".into();
        t.device = format!("device-{id}");
        t.state = TaskState::Queued;
        state.tasks.insert(id, t);
        state.results.insert(id, retention::ResultInfo::protected());
    }
    assert_eq!(state.budget.used(), 0);
    assert!(state.schedule_device("device-1"));
    assert!(!state.schedule_device("device-2"));
    assert_eq!(state.budget.used(), 2 * 1024 * 1024);
    state.tasks.get_mut(&1).unwrap().state = TaskState::Completed;
    state.release_dispatch(1);
    assert!(state.schedule_device("device-2"));
}

#[test]
#[ignore = "isolated Linux RSS stress run; execute with --ignored --exact --nocapture"]
fn mixed_response_memory_plateaus() {
    fn rss_kib() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse().ok())
            })
            .unwrap()
    }
    let rows = (0..6000).map(|n| format!("<ParameterInfoStruct><Name>InternetGatewayDevice.WANDevice.1.Test.{n}</Name><Writable>true</Writable></ParameterInfoStruct>")).collect::<String>();
    let large = format!("<cwmp:GetParameterNamesResponse><ParameterList>{rows}</ParameterList></cwmp:GetParameterNamesResponse>");
    let medium = format!("<cwmp:GetParameterValuesResponse><ParameterList>{}</ParameterList></cwmp:GetParameterValuesResponse>", (0..500).map(|n| format!("<ParameterValueStruct><Name>InternetGatewayDevice.Test.{n}</Name><Value xsi:type=\"xsd:string\">value</Value></ParameterValueStruct>")).collect::<String>());
    let mut state = State::default();
    let mut samples = Vec::new();
    let mut high_water = 0;
    let mut next = 0;
    for round in 0..40 {
        let mut outstanding = Vec::new();
        for device in 0..16 {
            next += 1;
            let mut t = task(next);
            t.device = format!("stress-{device}");
            t.method = match device % 4 {
                0 => "GetParameterNames",
                1 => "GetParameterValues",
                _ => "SetParameterValues",
            }
            .into();
            t.state = TaskState::Queued;
            t.sent = None;
            state.dispatch_demand(&t.device);
            state
                .results
                .insert(next, retention::ResultInfo::protected());
            state.tasks.insert(next, t);
            outstanding.push(next);
        }
        let mut consumers = std::collections::VecDeque::new();
        while !outstanding.is_empty() {
            // All live CPEs request work, with small and large replies mixed.
            let mut progress = false;
            for id in outstanding.clone() {
                let device = state.tasks[&id].device.clone();
                if !state.schedule_device(&device) {
                    continue;
                }
                high_water = high_water.max(state.budget.used());
                assert!(high_water <= resources::RESULT_BYTES);
                let t = state.tasks.get_mut(&id).unwrap();
                t.state = TaskState::Sent;
                t.sent = Some(now());
                let body = match t.method.as_str() { "GetParameterNames" => large.as_str(), "GetParameterValues" => medium.as_str(), _ => "<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>" };
                let xml = soap::envelope("urn:dslforum-org:cwmp-1-0", &format!("fttr-{id}"), body);
                let limit = state.parsing_permit(id).unwrap().bytes();
                let message = crate::runtime::block_on(soap::parse_stream(
                    tokio::io::BufReader::new(xml.as_bytes()),
                    |_| limit,
                ))
                .unwrap();
                state.response(&device, message).unwrap();
                assert_ne!(state.tasks[&id].state, TaskState::Fault);
                consumers.push_back(
                    state.tasks[&id]
                        .result
                        .as_ref()
                        .unwrap()
                        .reply(json!({"state":"completed"})),
                );
                assert!(state.ack_result(id));
                if consumers.len() > 2 {
                    consumers.pop_front();
                }
                outstanding.retain(|n| *n != id);
                progress = true;
            }
            if !progress {
                assert!(
                    consumers.pop_front().is_some(),
                    "fair scheduler stalled with no consumer holding capacity"
                );
            }
        }
        drop(consumers);
        assert_eq!(state.budget.used(), 0);
        state.tasks.clear();
        state.results.clear();
        if round % 5 == 4 {
            samples.push(rss_kib());
        }
    }
    println!("640 mixed-device RPCs; budget high-water={high_water} bytes; RSS KiB (5-round intervals)={samples:?}");
    let warm = samples[1];
    assert!(
        samples[2..].iter().all(|rss| *rss <= warm + 8 * 1024),
        "RSS failed to plateau after warmup"
    );
}

#[test]
fn queued_consumer_lease_starts_at_completion_not_queue_time() {
    let mut state = State::default();
    let mut t = task(1);
    t.state = TaskState::Queued;
    t.created = now() - 300;
    t.sent = None;
    state.tasks.insert(1, t);
    state.results.insert(1, retention::ResultInfo::protected());
    state.expire();
    assert!(state.results[&1].protected);
    assert_eq!(state.results[&1].completed_at, None);
    assert!(!state.ack_result(1));
    let t = state.tasks.get_mut(&1).unwrap();
    t.state = TaskState::Sent;
    t.sent = Some(now() - 60);
    let message = soap::parse(&soap::envelope(
        "urn:dslforum-org:cwmp-1-0",
        "fttr-1",
        "<cwmp:GetParameterValuesResponse/>",
    ))
    .unwrap();
    state.response("compact-test", message).unwrap();
    assert!(state.results[&1].protected);
    assert!(state.tasks[&1].result.is_some());
    assert!(state.results[&1].completed_at.unwrap() >= now() - 1);
}

#[test]
fn compact_wan_safety_evidence_survives_raw_cache_cleanup_but_expires() {
    let mut state = State::default();
    let mut t = task(1);
    t.sent = Some(now() - 60);
    state.tasks.insert(1, t);
    let prefix = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.2.WANIPConnection.1.";
    let mut message = soap::parse(&soap::envelope(
        "urn:dslforum-org:cwmp-1-0",
        "fttr-1",
        "<cwmp:GetParameterValuesResponse/>",
    ))
    .unwrap();
    message.data = serde_json::from_value(
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":"OTHER"}]}),
    )
    .unwrap();
    state.response("compact-test", message).unwrap();
    state.results.get_mut(&1).unwrap().completed_at = Some(now() - 30);
    state.prune();
    assert!(state.tasks[&1].result.is_none());
    assert_eq!(
        readback::observed(&state, "compact-test", prefix),
        Some(false)
    );
    state.results.get_mut(&1).unwrap().completed_at = Some(now() - 91);
    assert_eq!(readback::observed(&state, "compact-test", prefix), None);
    let evidence = readback::service_evidence(
        &serde_json::from_value(json!({"parameters":[
            {"name":format!("{prefix}X_CU_ServiceList"),"value":"OTHER"},
            {"name":format!("{prefix}X_CU_ServiceList"),"value":"INTERNET_TR069"}
        ]}))
        .unwrap(),
    );
    assert_eq!(evidence.get(prefix), Some(&true));
}

#[test]
fn oversized_save_preserves_previous_readable_state() {
    let server = server();
    server.save(&State::default()).unwrap();
    let before = fs::read(&server.state_file).unwrap();
    let mut state = State::default();
    let mut t = task(1);
    t.result = Some(Response::uncharged(
        json!({"padding":"x".repeat(8 * 1024 * 1024)}),
    ));
    state.tasks.insert(1, t);
    assert!(server.save(&state).is_err());
    assert_eq!(fs::read(&server.state_file).unwrap(), before);
    assert!(State::load(&server.state_file).is_ok());
    fs::remove_file(format!("{}.tmp", server.state_file)).unwrap();
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn large_result_survives_response_save_load_and_direct_query() {
    let server = server();
    let parameters: Vec<_> = (0..2_000)
        .map(|n| {
            json!({
                "name":format!("InternetGatewayDevice.LANDevice.1.Test.{n}"),
                "value":format!("{n}:\"\\\n{}", "x".repeat(170)), "type":"xsd:string"
            })
        })
        .collect();
    let expected = json!({"parameters":parameters});
    let mut state = State::default();
    state.tasks.insert(1, task(1));
    let mut message = soap::parse(&soap::envelope(
        "urn:dslforum-org:cwmp-1-0",
        "fttr-1",
        "<cwmp:GetParameterValuesResponse/>",
    ))
    .unwrap();
    message.data = serde_json::from_value(expected.clone()).unwrap();

    state.results.insert(1, retention::ResultInfo::protected());
    state.response("compact-test", message).unwrap();
    server.save(&state).unwrap();
    let loaded = State::load(&server.state_file).unwrap();
    *server.state.lock().unwrap() = loaded;
    let reply = server.execute(json!({"op":"task","id":1}));

    assert!(reply.len() > 400 * 1024);
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"], expected);
    assert_eq!(reply["state"], "completed");
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn result_budget_lives_until_last_shared_consumer_drops() {
    let mut state = State::default();
    let value = json!({"parameters":[], "padding":"x".repeat(100_000)});
    let permit = state
        .budget
        .reserve(resources::memory(&value), resources::RESULT_BYTES)
        .unwrap();
    let response = Response::charged(serde_json::from_value(value).unwrap(), permit);
    let reply = response.reply(json!({"state":"completed"}));
    let second = reply.clone();
    let mut record = task(1);
    record.state = TaskState::Completed;
    record.result = Some(response);
    state.tasks.insert(1, record);
    state.results.insert(1, retention::ResultInfo::protected());
    state.mark_completed(1);
    assert!(state.ack_result(1));
    assert!(state.budget.used() > 100_000);
    drop(reply);
    assert!(state.budget.used() > 100_000);
    drop(second);
    assert_eq!(state.budget.used(), 0);
}

#[test]
fn malformed_or_oversized_state_blocks_queries_mutation_and_replacement() {
    for contents in [b"{invalid".to_vec(), vec![b' '; 8 * 1024 * 1024 + 1]] {
        let server = server();
        fs::write(&server.state_file, &contents).unwrap();
        *server.state.lock().unwrap() = State::load_or_block(&server.state_file);

        let query: Value =
            serde_json::from_str(&server.execute(json!({"op":"task","id":1}))).unwrap();
        let reset: Value = serde_json::from_str(
            &server.execute(json!({"op":"reset_device_tasks","device":"compact-test"})),
        )
        .unwrap();
        let saved = server.save(&server.state.lock().unwrap());
        let activated = transition(server.clone(), true, "127.0.0.1:0", "/cwmp", "", "", false);

        assert!(query["error"].as_str().unwrap().starts_with("state load:"));
        assert!(reset["error"].as_str().unwrap().starts_with("state load:"));
        assert!(saved.is_err());
        assert!(activated.is_err());
        assert_eq!(fs::read(&server.state_file).unwrap(), contents);
        fs::remove_file(&server.state_file).unwrap();
    }
}

#[test]
fn restart_marks_sent_unknown_without_losing_completed_results() {
    let server = server();
    let mut state = State::default();
    state.tasks.insert(7, task(7));
    let mut completed = task(8);
    completed.state = TaskState::Completed;
    completed.result = Some(Response::uncharged(
        json!({"status":"1","instance_number":"12"}),
    ));
    state.tasks.insert(8, completed);
    server.save(&state).unwrap();

    let loaded = State::load(&server.state_file).unwrap();

    assert_eq!(loaded.tasks[&7].state, TaskState::Unknown);
    assert!(loaded.tasks[&7].result.as_ref().unwrap().get()["error"]
        .as_str()
        .unwrap()
        .contains("execution is unknown"));
    assert_eq!(
        loaded.tasks[&8].result.as_ref().unwrap().get(),
        state.tasks[&8].result.as_ref().unwrap().get()
    );
    assert_eq!(loaded.next_id, 8);
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn fault_and_add_object_fields_survive_raw_retention() {
    for (method, body, expected_state) in [
        ("SetParameterValues", "<soap:Fault><faultcode>Client</faultcode><faultstring>CWMP fault</faultstring><detail><cwmp:Fault><FaultCode>9003</FaultCode><FaultString>Invalid arguments</FaultString><SetParameterValuesFault><ParameterName>bad.name</ParameterName><FaultCode>9005</FaultCode><FaultString>Invalid parameter</FaultString></SetParameterValuesFault></cwmp:Fault></detail></soap:Fault>", TaskState::Fault),
        ("AddObject", "<cwmp:AddObjectResponse><InstanceNumber>12</InstanceNumber><Status>1</Status></cwmp:AddObjectResponse>", TaskState::AcceptedPendingApply),
    ] {
        let mut state = State::default();
        let mut record = task(1);
        record.method = method.into();
        state.tasks.insert(1, record);
        let message = soap::parse(&soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", body)).unwrap();
        let expected = message.data.clone();

        state.response("compact-test", message).unwrap();

        assert_eq!(state.tasks[&1].state, expected_state);
        assert_eq!(state.tasks[&1].result.as_ref().unwrap().get(), &expected);
    }
}

#[test]
fn parent_wan_evidence_survives_ack_and_persistence_without_retaining_body() {
    let parent = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.2.";
    let connection = format!("{parent}WANIPConnection.1.");
    for sibling in [None, Some(json!("TR069")), Some(json!(false))] {
        let mut state = State::default();
        state.tasks.insert(1, task(1));
        state.results.insert(1, retention::ResultInfo::protected());
        let mut rows =
            vec![json!({"name":format!("{connection}X_CU_ServiceList"),"value":"OTHER"})];
        let protected = sibling.is_some();
        if let Some(value) = sibling {
            rows.push(
                json!({"name":format!("{parent}WANIPConnection.2.X_CU_ServiceList"),"value":value}),
            );
        }
        let mut message = soap::parse(&soap::envelope(
            "urn:dslforum-org:cwmp-1-0",
            "fttr-1",
            "<cwmp:GetParameterValuesResponse/>",
        ))
        .unwrap();
        message.data = serde_json::from_value(json!({"parameters":rows})).unwrap();
        state.response("compact-test", message).unwrap();
        assert!(state.ack_result(1));
        assert!(state.tasks[&1].result.is_none());
        assert_eq!(state.budget.used(), 0);
        let mut restored: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(
            readback::observed(&restored, "compact-test", &connection),
            Some(false)
        );
        assert_eq!(
            readback::observed(&restored, "compact-test", parent),
            Some(protected)
        );
        assert_eq!(readback::observed(&restored, "other-device", parent), None);
        restored.results.get_mut(&1).unwrap().completed_at = Some(now() - 91);
        assert_eq!(readback::observed(&restored, "compact-test", parent), None);
    }
}

#[test]
fn wan_readback_guard_requires_eligible_fresh_non_management_result() {
    let prefix = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.1.";
    let mut state = State::default();
    let mut record = task(1);
    record.state = TaskState::Completed;
    record.result = Some(Response::uncharged(
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":"INTERNET"}]}),
    ));
    state.tasks.insert(1, record.clone());

    let observed = readback::observed(&state, "compact-test", prefix);

    assert_eq!(observed, Some(false));
    for (device, method, status, sent) in [
        ("other", "GetParameterValues", TaskState::Completed, now()),
        (
            "compact-test",
            "GetParameterNames",
            TaskState::Completed,
            now(),
        ),
        (
            "compact-test",
            "GetParameterValues",
            TaskState::Fault,
            now(),
        ),
        (
            "compact-test",
            "GetParameterValues",
            TaskState::Completed,
            now() - 91,
        ),
    ] {
        let mut ineligible = record.clone();
        ineligible.device = device.into();
        ineligible.method = method.into();
        ineligible.state = status;
        ineligible.sent = Some(sent);
        state.tasks.insert(1, ineligible);
        assert_eq!(readback::observed(&state, "compact-test", prefix), None);
    }
}

#[test]
fn wan_readback_guard_fails_closed_on_management_or_malformed_results() {
    let prefix = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.1.";
    for data in [
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":"INTERNET_TR069"}]}),
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":false}]}),
        json!({"parameters":null}),
    ] {
        let mut state = State::default();
        let mut record = task(1);
        record.state = TaskState::Completed;
        record.result = Some(Response::uncharged(data));
        state.tasks.insert(1, record);

        let observed = readback::observed(&state, "compact-test", prefix);

        assert_eq!(observed, Some(true));
    }
}

#[test]
fn test_prune_bounds_inactive_tasks_and_preserves_active() {
    let mut state = State::default();
    let current_time = now();

    // 50 completed tasks, half old (> 180s)
    for i in 1..=50 {
        let mut t = task(i);
        t.state = TaskState::Completed;
        t.created = if i <= 25 {
            current_time - 2000
        } else {
            current_time - 100
        };
        t.sent = Some(t.created);
        state.tasks.insert(i, t);
    }
    // 2 active tasks (one queued, one sent, both old)
    let mut active1 = task(101);
    active1.state = TaskState::Queued;
    active1.created = current_time - 2000;
    state.tasks.insert(101, active1);

    let mut active2 = task(102);
    active2.state = TaskState::Sent;
    active2.created = current_time - 2000;
    active2.sent = Some(current_time - 2000);
    state.tasks.insert(102, active2);

    state.prune();

    // Active tasks must remain
    assert!(state.tasks.contains_key(&101));
    assert!(state.tasks.contains_key(&102));

    // Old tasks (1..=25) must be pruned by TTL
    for i in 1..=25 {
        assert!(!state.tasks.contains_key(&i));
    }

    // Fresh inactive tasks (26..=50) remain
    let inactive_count = state
        .tasks
        .values()
        .filter(|t| !t.state.is_active())
        .count();
    assert_eq!(inactive_count, 25);
}

#[test]
fn timed_out_write_keeps_its_fence_without_retaining_request_arguments() {
    let mut state = State::default();
    let mut unresolved = task(101);
    unresolved.method = "SetParameterValues".into();
    unresolved.state = TaskState::Sent;
    unresolved.created = now().saturating_sub(2000);
    unresolved.sent = Some(unresolved.created);
    unresolved.args = json!({"parameters":[{"name":"Example.Password","value":"fixture"}]});
    state.tasks.insert(unresolved.id, unresolved);

    state.expire();
    assert_eq!(state.tasks[&101].state, TaskState::Unknown);
    assert!(state.tasks[&101].args.is_null());
    state.results.get_mut(&101).unwrap().completed_at = Some(now().saturating_sub(2000));
    state.prune();

    assert!(state.tasks.contains_key(&101));
    assert!(super::policy::protect_management(
        &state,
        "compact-test",
        "SetParameterValues",
        &json!({"parameters": []}),
    )
    .is_err());
    assert!(
        super::policy::protect_unresolved(&state, "compact-test", "GetParameterValues").is_ok()
    );
    assert!(
        super::policy::protect_unresolved(&state, "another-device", "SetParameterValues").is_ok()
    );
}

#[test]
fn test_prune_bounds_total_result_bytes_dropping_older_inactive_tasks() {
    let mut state = State::default();
    let current_time = now();

    let mut t1 = task(1);
    t1.state = TaskState::Completed;
    t1.created = current_time - 20;
    t1.sent = Some(t1.created);
    t1.result = Some(Response::uncharged(
        json!({"padding":"a".repeat(200 * 1024)}),
    ));
    state.tasks.insert(1, t1);

    let mut t2 = task(2);
    t2.state = TaskState::Completed;
    t2.created = current_time - 5;
    t2.sent = Some(t2.created);
    t2.result = Some(Response::uncharged(
        json!({"padding":"b".repeat(100 * 1024)}),
    ));
    state.tasks.insert(2, t2);

    state.prune();

    assert!(state.tasks[&1].result.is_none());
    assert!(state.tasks[&2].result.is_some());
}

#[test]
fn test_get_parameter_values_rejects_trailing_dot_wildcards() {
    let wildcard = json!({"names": ["InternetGatewayDevice."]});
    assert!(validate("GetParameterValues", &wildcard).is_err());

    let leaf = json!({"names": ["InternetGatewayDevice.DeviceInfo.SoftwareVersion"]});
    assert!(validate("GetParameterValues", &leaf).is_ok());
}

#[test]
fn test_clean_op_clears_inactive_tasks_and_resets_load_failure() {
    let server = server();
    let mut state = State::default();
    state.load_failure = Some("state load: ACS state exceeds 8 MiB".into());

    let mut inactive = task(1);
    inactive.state = TaskState::Completed;
    state.tasks.insert(1, inactive);

    let mut active = task(2);
    active.state = TaskState::Queued;
    state.tasks.insert(2, active);

    *server.state.lock().unwrap() = state;

    let res: Value = serde_json::from_str(&server.execute(json!({"op": "clean"}))).unwrap();
    assert_eq!(res["cleaned"], true);
    assert_eq!(res["tasks"], 1);

    let state = server.state.lock().unwrap();
    assert!(state.load_failure.is_none());
    assert!(!state.tasks.contains_key(&1));
    assert!(state.tasks.contains_key(&2));
    let _ = fs::remove_file(&server.state_file);
}
#[test]
fn ordinary_terminal_tasks_release_requests_but_keep_the_existing_response_lease() {
    let mut state = State::default();
    let mut row = super::Task {
        id: 13,
        device: "fixture-cpe".into(),
        method: "SetParameterValues".into(),
        args: serde_json::json!({"parameters":[{"name":"Device.Example.Password","value":"fixture-only"}]}),
        state: TaskState::Sent,
        created: now(),
        sent: Some(now()),
        result: None,
    };
    state.tasks.insert(13, row.clone());
    state.prune();
    assert_eq!(state.tasks[&13].args, row.args);
    row.state = TaskState::Completed;
    row.result = Some(error_result("fixture-result"));
    state.tasks.insert(13, row);
    state
        .results
        .insert(13, super::retention::ResultInfo::protected());
    state.mark_completed(13);
    state.prune();
    assert!(state.tasks[&13].args.is_null());
    assert!(state.tasks[&13].result.is_some());
    assert!(state.ack_result(13));
    assert!(state.tasks[&13].result.is_none());
}
