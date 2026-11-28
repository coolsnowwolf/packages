use fttr_api::{HlListReply, HlReportPhase, HlReportReply};
pub use fttr_api::{HlQuery as Query, HlSourceSelection as SourceSelection};
use serde::Serialize;
use serde_json::Value;

fn wire(value: impl Serialize) -> Value {
    serde_json::to_value(value).expect("HL202 wire models are JSON serializable")
}

#[derive(Serialize)]
pub struct Device {
    pub optical_sn: String,
    pub onu_id: u16,
    pub model: String,
    pub online: bool,
}

fn source_report(
    registry: &crate::mqtt_identity::Registry,
    target: &SourceSelection,
    now: u64,
) -> Value {
    match registry.snapshots(now).into_iter().find(|snapshot| {
        snapshot.mqtt_mac == target.mqtt_mac
            && snapshot.binding_generation == target.binding_generation
    }) {
        Some(snapshot) => super::telemetry::store::with_snapshot(
            &snapshot.mqtt_mac,
            |report| serde_json::json!({"schema_version": 1, "report": report, "target": &snapshot, "read_only": true}),
        ),
        None => error(
            "stale_source",
            "MQTT source observation changed or is no longer available",
        ),
    }
}

fn error(code: &str, message: &str) -> Value {
    wire(HlReportReply::Error {
        schema_version: 1,
        error: fttr_api::OpticalError {
            code: code.into(),
            message: message.into(),
        },
    })
}

pub fn control(input: &str, devices: &[Device]) -> Value {
    if input.len() > crate::hl202_ipc::MAX_REQUEST_BYTES {
        return error("invalid_request", "HL202 request exceeds the byte limit");
    }
    let query = match serde_json::from_str::<Query>(input) {
        Ok(query) => query,
        Err(_) => return error("invalid_request", "invalid HL202 request schema"),
    };
    control_query(query, devices)
}

pub fn control_query(query: Query, devices: &[Device]) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let registry = crate::mqtt_identity::shared();
    match query {
        Query::List => {
            let sources = match registry.lock() {
                Ok(state) => state.snapshots(now),
                Err(_) => return error("unavailable", "MQTT identity state unavailable"),
            };
            let rows: Vec<fttr_api::HlDevice> = devices
                .iter()
                .map(|device| {
                    let mqtt = sources.iter().find(|source| {
                        source.reason == "ready"
                            && source.optical_sn.as_deref() == Some(&device.optical_sn)
                            && source.onu_id == Some(device.onu_id)
                    });
                    let report_revision =
                        mqtt.and_then(|source| super::telemetry::store::revision(&source.mqtt_mac));
                    fttr_api::HlDevice {
                        optical_sn: device.optical_sn.clone(),
                        onu_id: device.onu_id,
                        model: device.model.clone(),
                        optical_online: device.online,
                        can_execute: cfg!(feature = "mqtt") && device.online && mqtt.is_some(),
                        mqtt: mqtt.cloned(),
                        report_revision,
                    }
                })
                .collect();
            wire(HlListReply::List {
                schema_version: 1,
                devices: rows,
                sources,
            })
        }
        Query::Report { target } => match registry.lock() {
            Ok(state) => match state.resolve(&target, now) {
                Ok(snapshot) => {
                    super::telemetry::store::with_snapshot(&snapshot.mqtt_mac, |report| {
                        let state = if report.is_some() {
                            HlReportPhase::Received
                        } else {
                            HlReportPhase::ReportUnavailable
                        };
                        serde_json::json!({"schema_version": 1, "target": &snapshot, "report": report, "state": state})
                    })
                }
                Err(message) => error("stale_target", message),
            },
            Err(_) => error("unavailable", "MQTT identity state unavailable"),
        },
        Query::SourceReport { target } => match registry.lock() {
            Ok(state) => source_report(&state, &target, now),
            Err(_) => error("unavailable", "MQTT identity state unavailable"),
        },
        Query::Run { request } => {
            #[cfg(not(feature = "mqtt"))]
            {
                let _ = request;
                error(
                    "mqtt_unavailable",
                    "MQTT commands unavailable: build requires mqtt",
                )
            }
            #[cfg(feature = "mqtt")]
            {
                if !devices.iter().any(|device| {
                    device.online
                        && device.optical_sn == request.target.optical_sn
                        && device.onu_id == request.target.onu_id
                        && device.model == "HL202-DU-C"
                }) {
                    return error(
                        "unsupported_target",
                        "selected device is not an online HL202",
                    );
                }
                let request = match super::request::validate(request) {
                    Ok(request) => request,
                    Err(_) => return error("invalid_request", "invalid HL202 action parameters"),
                };
                if !crate::provisioning::controller::external_write_allowed(
                    &request.target.optical_sn,
                ) || !crate::bootstrap::external_command_allowed(&request.target.optical_sn)
                {
                    return error(
                        "unresolved_transaction",
                        "device has an unresolved transaction or reset; command admission refused",
                    );
                }
                match request.action {
                    super::request::Action::Exec {
                        script,
                        timeout_seconds,
                    } => {
                        match super::command_binding::execute(
                            &request.target,
                            &script,
                            std::time::Duration::from_secs(u64::from(timeout_seconds)),
                        ) {
                            Ok(result) => wire(fttr_api::HlExecReply::Executed {
                                schema_version: 1,
                                operation: result,
                            }),
                            Err(cause) => error("command_failed", &cause.to_string()),
                        }
                    }
                    action @ (super::request::Action::AccountsRead
                    | super::request::Action::AccountSetPassword { .. }
                    | super::request::Action::AccountSetActive { .. }) => {
                        match super::accounts::run(&request.target, &action) {
                            Ok(result) => wire(result),
                            Err(_) => error(
                                "account_failed",
                                "account command failed or returned unsupported metadata",
                            ),
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hl202_request_unbound_source_report_requires_its_own_generation() {
        let mut registry = crate::mqtt_identity::Registry::new("source-test".into());
        registry.update_topology(Default::default(), Ok(Default::default()), 10);
        registry.observe("020000000001", 10);
        let snapshot = registry.snapshots(10).pop().unwrap();
        let target = SourceSelection {
            mqtt_mac: snapshot.mqtt_mac,
            binding_generation: snapshot.binding_generation,
        };
        let reply = source_report(&registry, &target, 10);
        assert_eq!(reply["target"]["reason"], "unmatched");
        assert!(reply.get("operation").is_none());
        let other = SourceSelection {
            mqtt_mac: "020000000002".into(),
            ..target
        };
        assert_eq!(
            source_report(&registry, &other, 10)["error"]["code"],
            "stale_source"
        );
    }

    #[test]
    fn hl202_request_api_rejects_malformed_without_returning_body() {
        let reply = control("{private-credential", &[]);
        assert_eq!(reply["error"]["code"], "invalid_request");
        assert!(!reply.to_string().contains("private-credential"));
    }

    #[test]
    fn hl202_request_api_lists_optical_device_without_mqtt_as_unavailable() {
        let devices = vec![Device {
            optical_sn: "H3CT12345678".into(),
            onu_id: 2,
            model: "HL202-DU-C".into(),
            online: true,
        }];
        let reply = control("{\"method\":\"list\"}", &devices);
        assert_eq!(reply["devices"][0]["optical_sn"], "H3CT12345678");
        assert_eq!(reply["devices"][0]["can_execute"], false);
        assert_eq!(reply["devices"][0]["mqtt"], Value::Null);
    }
}
