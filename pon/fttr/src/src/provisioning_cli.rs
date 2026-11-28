use serde_json::{json, Value};
use std::io;
use std::time::{Duration, Instant};
pub fn run(command: &str, args: &[String]) -> io::Result<i32> {
    let usage="fttrctl config check|reload\nfttrctl sync plan|apply|reconcile|recover|retry [DEVICE|all] [--item SECTION] [--wait SECONDS] [--management]\nfttrctl sync observe DEVICE [--wait SECONDS]\nfttrctl sync reset_state DEVICE\nfttrctl sync management_plan|management_apply\nfttrctl sync status|operation ID";
    let mut values = Vec::new();
    let mut wait = 0u64;
    let mut management = false;
    let mut item = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{usage}");
                return Ok(0);
            }
            "--json" => (),
            "--management" => management = true,
            "--item" => {
                i += 1;
                item = Some(
                    args.get(i)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidInput, "item section required")
                        })?
                        .clone(),
                );
            }
            "--wait" => {
                i += 1;
                wait = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .filter(|v| (1..=900).contains(v))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "wait must be 1..900")
                    })?;
            }
            s if s.starts_with('-') => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, usage))
            }
            _ => values.push(args[i].clone()),
        }
        i += 1;
    }
    let op = values.first().map(String::as_str).unwrap_or("");
    let request = match (command, op, values.len()) {
        ("config", "check" | "reload", 1) | ("sync", "status", 1) => json!({"op":op}),
        ("sync", "plan" | "apply" | "reconcile" | "retry", 1..=2) => {
            json!({"op":op,"target":values.get(1).map(String::as_str).unwrap_or("all"),"management":management,"item":item})
        }
        ("sync", "recover", 1..=2) if !management && item.is_none() => {
            json!({"op":op,"target":values.get(1).map(String::as_str).unwrap_or("all")})
        }
        ("sync", "observe" | "recover_reboot", 2) if !management && item.is_none() => {
            json!({"op":op,"target":values[1]})
        }
        ("sync", "reset_state", 2) if !management && item.is_none() && wait == 0 => {
            json!({"op":op,"target":values[1]})
        }
        ("sync", "management_plan" | "management_apply", 1..=2)
            if !management && item.is_none() && wait == 0 =>
        {
            json!({"op":op})
        }
        ("sync", "operation", 2) => {
            json!({"op":op,"id":values[1].parse::<u64>().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput,"invalid operation ID"))?})
        }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, usage)),
    };
    let mut reply = send(request)?;
    let mut timeout = false;
    if wait > 0 {
        if let Some(ids) = reply["operation_ids"].as_array().cloned() {
            let deadline = Instant::now() + Duration::from_secs(wait);
            loop {
                let mut operations = Vec::new();
                for id in &ids {
                    operations.push(send(json!({"op":"operation","id":id}))?);
                }
                let done = operations.iter().all(|o| o["state"] == "completed");
                reply = json!({"operations":operations});
                if done {
                    break;
                }
                if Instant::now() >= deadline {
                    timeout = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&reply)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    );
    let failed = reply.get("error").is_some_and(|v| !v.is_null())
        || reply["operations"].as_array().is_some_and(|a| {
            a.iter().any(|o| {
                o["observation_error"].is_string()
                    || ["wan_lifecycle", "wifi_lifecycle"].iter().any(|key| {
                        o[*key].as_object().is_some_and(|m| {
                            m.values().any(|i| {
                                matches!(
                                    i["state"].as_str(),
                                    Some("failed" | "blocked" | "unknown")
                                )
                            })
                        })
                    })
                    || o["items"].as_array().is_some_and(|items| {
                        items.iter().any(|i| {
                            matches!(
                                i["state"].as_str(),
                                Some(
                                    "failed"
                                        | "blocked"
                                        | "unknown"
                                        | "accepted_pending_apply"
                                        | "accepted_unverified"
                                )
                            )
                        })
                    })
            })
        });
    Ok(if failed {
        1
    } else if timeout {
        2
    } else {
        0
    })
}
fn send(v: Value) -> io::Result<Value> {
    let reply = crate::ipc::send_ipc_command(&format!("PROVISIONING {v}"))?;
    serde_json::from_str(&reply).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
