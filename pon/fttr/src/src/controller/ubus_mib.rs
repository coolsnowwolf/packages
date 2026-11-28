//! Registration-bound MIB jobs shared by platform ubus servers.
use crate::{
    controller::{inventory, service},
    platform::context::PlatformContext,
    pon_engine::{ploam::OnuState, PonEngine},
};
use std::sync::{Arc, Mutex};

/// Legacy numeric ubus failure code kept stable for existing clients: 9 marks a
/// generic internal failure (busy lock, unavailable engine/service state), as
/// opposed to the per-request validation code (2 = unsupported ONU-ID).
pub(crate) const UBUS_ERROR_INTERNAL: i32 = 9;

pub(crate) fn start(
    platform: PlatformContext,
    engine: Arc<Mutex<PonEngine>>,
    services: service::States,
    jobs: &fttr_jobs::Jobs,
    request: fttr_api::MibStartRequest,
) -> Result<fttr_api::JobStartReply, i32> {
    if !platform.resources().onu_ids.contains(&request.onu_id) {
        return Err(2);
    }
    let current = engine.try_lock().map_err(|_| UBUS_ERROR_INTERNAL)?;
    if !current.active_onus.iter().any(|onu| {
        onu.onu_id == request.onu_id
            && onu.full_sn_str() == request.optical_sn
            && onu.state == OnuState::O5Operation
    }) {
        return serde_json::from_value(fttr_jobs::error(
            "Device registration changed; reopen device details",
        ))
        .map_err(|_| UBUS_ERROR_INTERNAL);
    }
    let generation = services
        .try_lock()
        .map_err(|_| UBUS_ERROR_INTERNAL)?
        .get(&request.onu_id)
        .map(|s| s.generation)
        .ok_or(UBUS_ERROR_INTERNAL)?;
    drop(current);
    let states = services.clone();
    let id = request.onu_id;
    let guard: inventory::RegistrationGuard = Arc::new(move || {
        let Ok(engine) = engine.lock() else {
            return false;
        };
        let Ok(services) = services.lock() else {
            return false;
        };
        engine.active_onus.iter().any(|onu| {
            onu.onu_id == id
                && onu.full_sn_str() == request.optical_sn
                && onu.state == OnuState::O5Operation
        }) && services
            .get(&id)
            .is_some_and(|s| s.generation == generation)
    });
    serde_json::from_value(jobs.start_with_progress("mib", move |progress| {
        progress.update(fttr_jobs::Phase::Starting, 0, None);
        match inventory::control_guarded(
            platform,
            id,
            request.tables,
            Some(inventory::MibProgressReporter::Job(progress)),
            &states,
            Some(guard.clone()),
        ) {
            Ok(report) if guard() => match serde_json::to_value(report)
                .and_then(serde_json::from_value::<fttr_api::MibReport>)
            {
                Ok(report) => serde_json::to_value(fttr_api::MibSnapshot {
                    status: fttr_api::MibReady::Ready,
                    report,
                })
                .expect("MIB snapshot is JSON serializable"),
                Err(_) => fttr_jobs::error("Invalid MIB report contract"),
            },
            Ok(_) => fttr_jobs::error("Device registration changed during MIB read"),
            Err(error) => fttr_jobs::error(&error.to_string()),
        }
    }))
    .map_err(|_| UBUS_ERROR_INTERNAL)
}
