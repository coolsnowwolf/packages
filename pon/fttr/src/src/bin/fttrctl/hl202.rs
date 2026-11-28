use openwrt_fttr::devices::hl202::api::{Query, SourceSelection};
use openwrt_fttr::{hl202_ipc, mqtt_identity::Selection};
use std::path::Path;
use std::time::Duration;

fn parse(args: &[String]) -> Result<Query, String> {
    let usage = "Usage: fttrctl hl202 list --json | report --optical-sn SN --onu-id N --generation G --json | run --request-file PATH --json";
    let method = args.first().ok_or(usage)?;
    let mut json = false;
    let mut serial = None;
    let mut onu = None;
    let mut generation = None;
    let mut path = None;
    let mut source = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if !json => json = true,
            flag @ ("--optical-sn" | "--onu-id" | "--generation" | "--request-file"
            | "--source") => {
                index += 1;
                let value = args.get(index).ok_or(usage)?.clone();
                let slot = match flag {
                    "--optical-sn" => &mut serial,
                    "--onu-id" => &mut onu,
                    "--generation" => &mut generation,
                    "--source" => &mut source,
                    _ => &mut path,
                };
                if slot.replace(value).is_some() {
                    return Err(usage.into());
                }
            }
            _ => return Err(usage.into()),
        }
        index += 1;
    }
    if !json {
        return Err(usage.into());
    }
    match method.as_str() {
        "list"
            if serial.is_none()
                && onu.is_none()
                && generation.is_none()
                && path.is_none()
                && source.is_none() =>
        {
            Ok(Query::List)
        }
        "source-report" if path.is_none() && serial.is_none() && onu.is_none() => {
            Ok(Query::SourceReport {
                target: SourceSelection {
                    mqtt_mac: source.filter(|value| !value.is_empty()).ok_or(usage)?,
                    binding_generation: generation
                        .filter(|value| !value.is_empty())
                        .ok_or(usage)?,
                },
            })
        }
        "report" if path.is_none() && source.is_none() => {
            let target = Selection {
                optical_sn: serial.filter(|value| !value.is_empty()).ok_or(usage)?,
                onu_id: onu.ok_or(usage)?.parse::<u16>().map_err(|_| usage)?,
                binding_generation: generation.filter(|value| !value.is_empty()).ok_or(usage)?,
            };
            if !openwrt_fttr::pon_engine::onu_id::is_valid(target.onu_id) {
                return Err(usage.into());
            }
            Ok(Query::Report { target })
        }
        "run" if serial.is_none() && onu.is_none() && generation.is_none() && source.is_none() => {
            let path = path.ok_or(usage)?;
            let request = openwrt_fttr::devices::hl202::request::read_file(Path::new(&path))
                .map_err(|error| error.to_string())?;
            Ok(Query::Run { request })
        }
        _ => Err(usage.into()),
    }
}

pub fn handle(args: &[String]) -> Result<i32, String> {
    let query = parse(args)?;
    let timeout = match &query {
        Query::Run { request } => match &request.action {
            openwrt_fttr::devices::hl202::request::Action::Exec {
                timeout_seconds, ..
            } => u64::from(*timeout_seconds) + 15,
            _ => 45,
        },
        _ => 10,
    };
    let body = serde_json::to_string(&query).map_err(|error| error.to_string())?;
    let response =
        hl202_ipc::send(&body, Duration::from_secs(timeout)).map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&response).map_err(|_| "invalid HL202 daemon response")?;
    println!("{response}");
    let failed = value.get("error").is_some()
        || value.get("operation").is_some_and(|operation| {
            let result = &operation["result"];
            result["status"] != "executed"
                || result["exit_code"] != 0
                || result.get("error").is_some_and(|error| !error.is_null())
        });
    Ok(i32::from(failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hl202_request_cli_requires_all_selection_fields() {
        assert!(matches!(
            parse(&["list".into(), "--json".into()]).unwrap(),
            Query::List
        ));
        assert!(parse(&[
            "report".into(),
            "--optical-sn".into(),
            "H3CT12345678".into(),
            "--json".into()
        ])
        .is_err());
    }
}
