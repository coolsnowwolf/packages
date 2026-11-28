use crate::devices::hl202::api::{self, Device};
use crate::pon_engine::{ploam::OnuState, PonEngine};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

static WORKERS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();

pub(crate) fn start(
    mut stream: UnixStream,
    input: String,
    engine: Arc<Mutex<PonEngine>>,
    services: super::service::States,
) {
    let Ok(permit) = WORKERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
    else {
        let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"busy\",\"message\":\"HL202 workers busy\"}}");
        return;
    };
    crate::runtime::spawn_blocking(move || {
        let _permit = permit;
        let response = control(&input, &engine, &services);
        let encoded = response.to_string();
        if encoded.len() <= crate::hl202_ipc::MAX_RESPONSE_BYTES {
            let _ = stream.write_all(encoded.as_bytes());
        } else {
            let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"response_limit\",\"message\":\"HL202 response exceeds byte limit\"}}");
        }
    });
}

pub(crate) fn control(
    input: &str,
    engine: &Arc<Mutex<PonEngine>>,
    services: &super::service::States,
) -> serde_json::Value {
    if input.len() > crate::hl202_ipc::MAX_REQUEST_BYTES {
        return serde_json::json!({"schema_version":1,"error":{"code":"invalid_request","message":"HL202 request exceeds the byte limit"}});
    }
    match serde_json::from_str(input) {
        Ok(query) => control_query(query, engine, services),
        Err(_) => {
            serde_json::json!({"schema_version":1,"error":{"code":"invalid_request","message":"invalid HL202 request schema"}})
        }
    }
}

pub(crate) fn control_query(
    query: fttr_api::HlQuery,
    engine: &Arc<Mutex<PonEngine>>,
    services: &super::service::States,
) -> serde_json::Value {
    match (engine.lock(), services.lock()) {
        (Ok(engine), Ok(services)) => {
            let devices: Vec<Device> = engine
                .active_onus
                .iter()
                .filter_map(|onu| {
                    let model = services
                        .get(&onu.onu_id)
                        .and_then(|service| service.identity.as_ref())
                        .and_then(|identity| identity.equipment_id.clone())
                        .unwrap_or_else(|| onu.model.clone());
                    if onu.vendor_str() != "H3CT" || model != "HL202-DU-C" {
                        return None;
                    }
                    Some(Device {
                        optical_sn: onu.full_sn_str(),
                        onu_id: u16::from(onu.onu_id),
                        model,
                        online: onu.state == OnuState::O5Operation,
                    })
                })
                .collect();
            drop(services);
            drop(engine);
            api::control_query(query, &devices)
        }
        _ => {
            serde_json::json!({"schema_version":1,"error":{"code":"unavailable","message":"optical identity state unavailable"}})
        }
    }
}
