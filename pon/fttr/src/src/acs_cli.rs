//! IPC front-end is available even when the ACS server is compiled out.
use serde_json::{json, Value};
use std::io;
use std::time::{Duration, Instant};
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn send(request: Value) -> io::Result<Value> {
    let reply = crate::ipc::send_ipc_command(&format!("ACS {request}"))?;
    serde_json::from_str(&reply).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn run(args: &[String]) -> io::Result<i32> {
    let usage = "fttrctl acs status|devices|clean|task ID\nfttrctl acs methods DEVICE [--wait SECONDS]\nfttrctl acs names DEVICE PATH [--next-level] [--wait SECONDS]\nfttrctl acs get DEVICE PATH... [--wait SECONDS]\nfttrctl acs set DEVICE PATH --type string|boolean|int|unsignedInt --value VALUE [--wait SECONDS]\nfttrctl acs add|delete DEVICE OBJECT. [--wait SECONDS]";
    let mut values = Vec::new();
    let mut wait = None;
    let mut next = false;
    let mut kind = "string".to_string();
    let mut value = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{usage}");
                return Ok(0);
            }
            "--next-level" => next = true,
            "--wait" | "--type" | "--value" => {
                let key = &args[i];
                i += 1;
                let v = args.get(i).ok_or_else(|| invalid("missing option value"))?;
                match key.as_str() {
                    "--wait" => {
                        let seconds: u64 = v.parse().map_err(|_| invalid("invalid wait"))?;
                        if !(1..=300).contains(&seconds) {
                            return Err(invalid("wait must be 1..300 seconds"));
                        }
                        wait = Some(seconds);
                    }
                    "--type" => kind = v.clone(),
                    _ => value = Some(v.clone()),
                }
            }
            "--json" => (),
            option if option.starts_with("--") => return Err(invalid("unknown ACS option")),
            _ => values.push(args[i].clone()),
        }
        i += 1;
    }
    let op = values.first().map(String::as_str).unwrap_or("");
    let request = match op {
        "status" | "devices" | "clean" if values.len() == 1 => json!({"op":op}),
        "task" if values.len() == 2 => {
            json!({"op":"task","id":values[1].parse::<u64>().map_err(|_| invalid("invalid task ID"))?})
        }
        "methods" if values.len() == 2 => {
            json!({"op":"queue","device":values[1],"method":"GetRPCMethods","args":{}})
        }
        "add" | "delete" if values.len() == 3 => {
            json!({"op":"queue","device":values[1],"method":if op == "add" {"AddObject"} else {"DeleteObject"},"args":{"object":values[2]}})
        }
        "names" if values.len() == 3 => {
            json!({"op":"queue","device":values[1],"method":"GetParameterNames","args":{"path":values[2],"next_level":next}})
        }
        "get" if values.len() >= 3 => {
            json!({"op":"queue","device":values[1],"method":"GetParameterValues","args":{"names":values[2..]}})
        }
        "set" if values.len() == 3 => {
            json!({"op":"queue","device":values[1],"method":"SetParameterValues","args":{"parameters":[{"name":values[2],"value":value.ok_or_else(|| invalid("--value required"))?,"kind":kind}]}})
        }
        _ => return Err(invalid(usage)),
    };
    let mut reply = send(request)?;
    if let (Some(seconds), Some(id)) = (wait, reply["id"].as_u64()) {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while matches!(reply["state"].as_str(), Some("queued" | "sent"))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(300));
            reply = send(json!({"op":"task","id":id}))?;
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&reply)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    );
    Ok(
        if reply.get("error").is_some_and(|v| !v.is_null())
            || matches!(
                reply["state"].as_str(),
                Some("fault" | "unknown" | "expired")
            )
        {
            1
        } else if wait.is_some() && matches!(reply["state"].as_str(), Some("queued" | "sent")) {
            2
        } else {
            0
        },
    )
}
