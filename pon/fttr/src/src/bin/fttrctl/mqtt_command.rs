use openwrt_fttr::devices::hl202::{
    command::{self, CommandResult},
    script,
};
use openwrt_fttr::{hl202_ipc, ipc, model::FttrSystemStatus};
use std::{path::Path, time::Duration};

fn usage() {
    eprintln!("Usage:\n  fttrctl exec [--timeout SECONDS] [--json] <mac> 'shell command'\n  fttrctl exec --script-file PATH [--timeout SECONDS] [--json] <mac>\n  fttrctl cmd --wait [--json] <mac> 'shell command'\n  fttrctl broadcast --wait [--targets MAC,MAC] [--timeout SECONDS] [--json] 'shell command'\n\nOptions must precede the shell command; -- ends option parsing.\nThe target is the child br0 MAC, not its optical SN. Default timeout: 5 seconds (1..300).\nAll commands require a fresh optical binding and daemon authorization.\nBroadcast --wait executes sequentially for individually authorized targets.\nUnconfirmed dispatch is disabled. Timeout does not cancel execution or permit replay.");
}

pub fn handle(args: &[String]) -> Result<i32, String> {
    let mode = args.first().ok_or("missing command mode")?.as_str();
    let broadcast = mode == "broadcast";
    let mut wait = mode == "exec";
    let mut json = false;
    let mut seconds = 5u64;
    let mut target = None;
    let mut targets = None;
    let mut script_file = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                usage();
                return Ok(0);
            }
            "--wait" => wait = true,
            "--no-wait" => wait = false,
            "--json" | "-j" => json = true,
            "--timeout" => {
                i += 1;
                seconds = args
                    .get(i)
                    .ok_or("--timeout needs seconds")?
                    .parse()
                    .map_err(|_| "invalid timeout")?;
                if !(1..=300).contains(&seconds) {
                    return Err("timeout must be 1..300 seconds".into());
                }
            }
            "--script-file" => {
                if mode != "exec" || script_file.is_some() {
                    return Err("--script-file is accepted once, for exec only".into());
                }
                i += 1;
                script_file = Some(args.get(i).ok_or("--script-file needs a path")?);
            }
            "--targets" => {
                i += 1;
                if !broadcast {
                    return Err("--targets is only valid for broadcast --wait".into());
                }
                targets = Some(
                    args.get(i)
                        .ok_or("--targets needs comma-separated MACs")?
                        .clone(),
                );
            }
            "--" => {
                i += 1;
                break;
            }
            option if option.starts_with('-') => return Err(format!("unknown option: {option}")),
            _ if !broadcast && target.is_none() => target = Some(args[i].clone()),
            _ => break,
        }
        i += 1;
    }
    if (!broadcast && target.is_none()) || (script_file.is_none() && i >= args.len()) {
        usage();
        return Err("missing target or shell command".into());
    }
    if targets.is_some() && !wait {
        return Err("--targets requires --wait".into());
    }
    if !wait {
        return Err(
            "unconfirmed dispatch is disabled; use exec or cmd --wait with daemon authorization"
                .into(),
        );
    }
    let source = match script_file {
        Some(path) => {
            if i != args.len() {
                return Err("provide either --script-file or a positional shell command".into());
            }
            script::read_file(Path::new(path))
                .map_err(|e| format!("cannot read script input: {e}"))?
        }
        None => script::validate(args[i..].join(" ")).map_err(|e| e.to_string())?,
    };
    let devices = if broadcast && wait {
        let values = if let Some(values) = targets {
            values.split(',').map(str::to_string).collect::<Vec<_>>()
        } else {
            let reply = ipc::send_ipc_command("STATUS")
                .map_err(|e| format!("cannot discover devices: {e}; use --targets"))?;
            let status: FttrSystemStatus = serde_json::from_str(&reply)
                .map_err(|e| format!("invalid STATUS response: {e}"))?;
            status
                .telemetry_gateways
                .into_iter()
                .filter(|d| d.last_seen_sec_ago <= 15)
                .map(|d| d.id.split('/').next().unwrap_or("").to_string())
                .collect()
        };
        let mut macs = Vec::new();
        for value in values {
            let mac = command::normalize_mac(&value).map_err(|e| e.to_string())?;
            if !macs.contains(&mac) {
                macs.push(mac);
            }
        }
        if macs.is_empty() {
            return Err("no fresh telemetry devices; specify --targets MAC,MAC".into());
        }
        macs
    } else if broadcast {
        vec!["all".to_string()]
    } else {
        vec![command::normalize_mac(&target.ok_or("missing target")?).map_err(|e| e.to_string())?]
    };
    let mut results = Vec::new();
    for device in devices {
        let result = daemon_execute(&device, &source, seconds)
            .unwrap_or_else(|e| CommandResult::failed(&device, e));
        if !json {
            if let Some(output) = &result.output {
                print!("{output}");
            }
            if let Some(error) = &result.error {
                eprintln!("[-] {} {}: {error}", result.target, result.status);
            } else if let Some(code) = result.exit_code {
                eprintln!(
                    "[{}] {} executed, exit={code}",
                    if code == 0 { "+" } else { "-" },
                    result.target
                );
            } else {
                println!(
                    "[+] {} dispatched (device execution not confirmed)",
                    result.target
                );
            }
        }
        results.push(result);
    }
    let code = if results.iter().all(CommandResult::success) {
        0
    } else if results.len() == 1 {
        results[0]
            .exit_code
            .filter(|c| (1..=255).contains(c))
            .unwrap_or(1)
    } else {
        1
    };
    if json {
        let output = if broadcast {
            serde_json::to_string(&results)
        } else {
            serde_json::to_string(&results[0])
        }
        .map_err(|e| e.to_string())?;
        println!("{output}");
    }
    Ok(code)
}

fn daemon_execute(target: &str, script: &str, seconds: u64) -> std::io::Result<CommandResult> {
    daemon_execute_with(target, script, seconds, |value, timeout| {
        hl202_ipc::check_request_size(&value)?;
        let body = hl202_ipc::send(&value.to_string(), timeout)?;
        serde_json::from_str(&body).map_err(std::io::Error::other)
    })
}

fn daemon_execute_with(
    target: &str,
    script: &str,
    seconds: u64,
    mut exchange: impl FnMut(serde_json::Value, Duration) -> std::io::Result<serde_json::Value>,
) -> std::io::Result<CommandResult> {
    use serde_json::json;
    let listing = exchange(json!({"method":"list"}), Duration::from_secs(10))?;
    let row = listing["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| {
            row["can_execute"] == true
                && row["mqtt"]["mqtt_mac"]
                    .as_str()
                    .is_some_and(|mac| mac.eq_ignore_ascii_case(target))
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fresh authorized HL202 binding required",
            )
        })?;
    let reply = exchange(
        json!({"method":"run","request":{"schema_version":1,"target":{
        "optical_sn":row["optical_sn"],"onu_id":row["onu_id"],"binding_generation":row["mqtt"]["binding_generation"]
    },"action":{"kind":"exec","script":script,"timeout_seconds":seconds}}}),
        Duration::from_secs(seconds + 15),
    )?;
    if reply["operation"]["result"].is_object() {
        if reply["operation"]["binding_current"] != true {
            return Err(std::io::Error::other(
                "command identity changed; execution not established; intent remains unresolved",
            ));
        }
        serde_json::from_value(reply["operation"]["result"].clone()).map_err(std::io::Error::other)
    } else {
        Err(std::io::Error::other(
            reply["error"]["message"]
                .as_str()
                .unwrap_or("daemon command failed"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn daemon_exec_uses_fresh_typed_selection_and_preserves_output_and_exit() {
        let mut calls = 0;
        let script = "printf '猫';\n$(fixture-only)";
        let result = daemon_execute_with("020000000071", script, 9, |value, timeout| {
            let query: fttr_api::HlQuery = serde_json::from_value(value).unwrap();
            calls += 1;
            match query {
                fttr_api::HlQuery::List => Ok(json!({"devices":[{"can_execute":true,"optical_sn":"TEST00000071","onu_id":7,"mqtt":{"mqtt_mac":"020000000071","binding_generation":"test-generation"}}]})),
                fttr_api::HlQuery::Run { request } => {
                    assert_eq!(request.target.optical_sn, "TEST00000071");
                    assert_eq!(request.target.onu_id, 7);
                    assert_eq!(request.target.binding_generation, "test-generation");
                    assert_eq!(timeout, Duration::from_secs(24));
                    let fttr_api::HlAction::Exec { script: received, timeout_seconds } = request.action else { panic!("exec required") };
                    assert_eq!(received, script);
                    assert_eq!(timeout_seconds, 9);
                    Ok(json!({"operation":{"binding_current":true,"result":{"target":"020000000071","status":"executed","exit_code":7,"output":"猫\nfixture stderr\n","output_truncated":false}}}))
                }
                _ => panic!("unexpected command"),
            }
        }).unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.output.as_deref(), Some("猫\nfixture stderr\n"));
        assert!(!result.success());
    }

    #[test]
    fn daemon_exec_never_reports_success_from_a_rebound_or_unverified_selection() {
        for binding_current in [serde_json::Value::Bool(false), serde_json::Value::Null] {
            let result = daemon_execute_with("020000000071", "printf fixture", 9, |value, _| {
                if value["method"] == "list" {
                    Ok(
                        json!({"devices":[{"can_execute":true,"optical_sn":"TEST00000071","onu_id":7,"mqtt":{"mqtt_mac":"020000000071","binding_generation":"original"}}]}),
                    )
                } else {
                    Ok(
                        json!({"operation":{"binding_current":binding_current,"result":{"target":"020000000071","status":"executed","exit_code":0,"output":"fixture","output_truncated":false}}}),
                    )
                }
            });
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("intent remains unresolved"));
        }
    }
}
