//! HM2004-DU daemon handlers driven by shared wire contracts.
use super::{OpticalClient, OpticalCommand, OpticalReplyExt};
use crate::controller::ubus_mib::UBUS_ERROR_INTERNAL;
use fttr_api::*;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::atomic::Ordering;
mod metrics;
#[cfg(test)]
mod tests;
#[path = "../../ubus_topology.rs"]
mod topology;

fn bind<Q: DeserializeOwned + 'static, R: Serialize + 'static>(
    method: &Method<Q, R>,
    handler: impl Fn(Q) -> Result<R, i32> + Send + Sync + 'static,
) -> (String, openwrt_ubus::MethodHandler) {
    let name = method.name;
    (
        method.name.into(),
        Box::new(move |_, message| {
            let started = std::time::Instant::now();
            let result = (|| {
                let mut payload = message.cloned().unwrap_or_else(|| serde_json::json!({}));
                // uhttpd adds authenticated session metadata to forwarded calls.
                // It is transport-owned, not part of the typed business request.
                if let Some(object) = payload.as_object_mut() {
                    object.remove("ubus_rpc_session");
                }
                let request = serde_json::from_value(payload).map_err(|_| 2)?;
                serde_json::to_value(handler(request)?).map_err(|_| UBUS_ERROR_INTERNAL)
            })();
            metrics::record(name, started.elapsed(), result.is_err());
            result
        }),
    )
}

fn optical(optics: &OpticalClient, command: OpticalCommand) -> Result<OpticalReply, i32> {
    let response = match optics.request(command) {
        Ok(response) => response,
        Err(error) => OpticalReply::unavailable(
            command,
            optics.hardware_ready.load(Ordering::Acquire),
            error,
        ),
    };
    serde_json::from_value::<OpticalReply>(response).map_err(|_| UBUS_ERROR_INTERNAL)
}

#[derive(Clone)]
struct Handlers {
    optics: OpticalClient,
    devices: std::sync::Arc<std::sync::Mutex<Vec<crate::ipc::SubDevice>>>,
    engine: std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: crate::controller::service::States,
    jobs: fttr_jobs::Jobs,
    topology: std::sync::Arc<std::sync::Mutex<crate::controller::topology::Cache>>,
}

impl Handlers {
    fn fpga_diagnostic(&self, request: FpgaDiagnosticRequest) -> Result<FpgaDiagnosticReply, i32> {
        Ok(
            match super::fpga_diagnostic_snapshot(
                &request,
                &self.engine,
                &self.services,
                &self.optics,
            ) {
                Ok(snapshot) => FpgaDiagnosticReply::Success { snapshot },
                Err(error) => FpgaDiagnosticReply::Error {
                    error: error.to_string(),
                },
            },
        )
    }
    fn npu_set(&self, request: NpuSetRequest) -> Result<NpuSetReply, i32> {
        #[cfg(feature = "npu")]
        {
            Ok(
                match crate::npu::settings::set(
                    request.enabled,
                    Some(request.bridge.as_str()),
                    Some(request.wan_upstream),
                    Some(request.wan_downstream),
                    Some(request.onu_bridge),
                ) {
                    Ok(()) => NpuSetReply::Success {
                        enabled: request.enabled,
                    },
                    Err(error) => NpuSetReply::Error {
                        error: error.to_string(),
                    },
                },
            )
        }
        #[cfg(not(feature = "npu"))]
        {
            let _ = request;
            Err(4)
        }
    }
    fn mib_start(&self, request: MibStartRequest) -> Result<JobStartReply, i32> {
        crate::controller::ubus_mib::start(
            self.optics.platform,
            self.engine.clone(),
            self.services.clone(),
            &self.jobs,
            request,
        )
    }
    fn mib_result(&self, request: JobRequest) -> Result<JobChunkReply, i32> {
        response(self.jobs.result(&request.job, request.chunk, Some("mib")))
    }
    fn job_result(&self, request: JobRequest) -> Result<JobChunkReply, i32> {
        response(self.jobs.result(&request.job, request.chunk, None))
    }
    fn job_release(&self, request: JobReleaseRequest) -> Result<JobReleaseReply, i32> {
        response(self.jobs.release(&request.job))
    }
    fn hl_query(&self, query: HlQuery) -> Result<serde_json::Value, i32> {
        #[cfg(feature = "device-hl202")]
        {
            crate::hl202_ipc::check_request_size(&query).map_err(|_| 2)?;
            if matches!(&query, HlQuery::List) {
                return Ok(crate::controller::hl202::control_query(
                    query,
                    &self.engine,
                    &self.services,
                ));
            }
            let handler = self.clone();
            Ok(self.jobs.start("hl202", move || {
                let result = crate::controller::hl202::control_query(
                    query.clone(),
                    &handler.engine,
                    &handler.services,
                );
                // Validate the final payload before publishing it into the job store.
                let result = normalize_hl_result(&query, result);
                result.unwrap_or_else(|_| fttr_jobs::error("Invalid HL202 result contract"))
            }))
        }
        #[cfg(not(feature = "device-hl202"))]
        {
            let _ = query;
            Err(4)
        }
    }
    fn hl202_list(&self, _: EmptyRequest) -> Result<HlListReply, i32> {
        response(self.hl_query(HlQuery::List)?)
    }
    fn hl202_report(&self, request: HlReportRequest) -> Result<JobStartReply, i32> {
        let query = match request.target {
            HlTarget::Optical(target) => HlQuery::Report { target },
            HlTarget::Source(target) => HlQuery::SourceReport { target },
        };
        response(self.hl_query(query)?)
    }
    fn hl202_accounts_start(&self, request: HlAccountsRequest) -> Result<JobStartReply, i32> {
        self.hl_run(request.target, HlAction::AccountsRead)
    }
    fn hl202_account_update(&self, request: HlUpdateRequest) -> Result<JobStartReply, i32> {
        let entry = u8::try_from(request.entry).map_err(|_| 2)?;
        let action = match request.kind {
            AccountChange::AccountSetPassword => HlAction::AccountSetPassword {
                entry,
                password: request.password,
            },
            AccountChange::AccountSetActive => HlAction::AccountSetActive {
                entry,
                active: request.active,
            },
        };
        self.hl_run(request.target, action)
    }
    fn hl202_exec_start(&self, request: HlExecRequest) -> Result<JobStartReply, i32> {
        self.hl_run(
            request.target,
            HlAction::Exec {
                script: request.script,
                timeout_seconds: request.timeout_seconds,
            },
        )
    }
    fn hl_run(&self, selection: HlTarget, action: HlAction) -> Result<JobStartReply, i32> {
        let HlTarget::Optical(target) = selection else {
            return Err(2);
        };
        response(self.hl_query(HlQuery::Run {
            request: HlCommandRequest {
                schema_version: 1,
                target,
                action,
            },
        })?)
    }
    fn get_calibration(&self, _: EmptyRequest) -> Result<CalibrationInfoReply, i32> {
        Ok(match super::calibration::get() {
            Ok(info) => CalibrationInfoReply::Info(info),
            Err(error) => CalibrationInfoReply::Error(CommandError {
                error: error.to_string(),
            }),
        })
    }
    fn select_calibration(
        &self,
        request: CalibrationSelectRequest,
    ) -> Result<CalibrationInfoReply, i32> {
        let result = (|| {
            let factory = crate::platform::h3c_hm2004_du::fmcs::read_factory_calibration();
            let firmware = match crate::platform::h3c_hm2004_du::calibration::firmware() {
                Ok(b) => b,
                Err(e) if request.source == CalibrationMode::Firmware => return Err(e),
                Err(_) => Vec::new(),
            };
            crate::platform::h3c_hm2004_du::calibration::select(
                request.source,
                factory.as_ref().map(|v| v.as_slice()),
                &firmware,
            )?;
            crate::optical::save_calibration_mode(request.source)?;
            super::calibration::get()
        })();
        Ok(match result {
            Ok(info) => CalibrationInfoReply::Info(info),
            Err(error) => CalibrationInfoReply::Error(CommandError {
                error: error.to_string(),
            }),
        })
    }
    fn save_calibration(
        &self,
        request: CalibrationSaveRequest,
    ) -> Result<CalibrationSaveReply, i32> {
        Ok(match super::calibration::save(&request.data) {
            Ok(saved) => CalibrationSaveReply::Saved(saved),
            Err(error) => CalibrationSaveReply::Error(CommandError {
                error: error.to_string(),
            }),
        })
    }
    fn apply_calibration(&self, _: EmptyRequest) -> Result<JobStartReply, i32> {
        let optics = self.optics.clone();
        response(self.jobs.start("calibration", move || {
            let result = match optical(&optics, OpticalCommand::Reinitialize) {
                Ok(reply) if reply.status == ReplyStatus::Success => CalibrationApplied {
                    state: super::calibration::get().ok().and_then(|i| i.state),
                    success: true,
                    output: Some(
                        if reply.desired_enabled == Some(true) {
                            "Calibration applied by optical owner"
                        } else {
                            "Optical TX remains disabled; calibration will load on next enable"
                        }
                        .into(),
                    ),
                    error: None,
                },
                Ok(reply) => CalibrationApplied {
                    state: super::calibration::get().ok().and_then(|i| i.state),
                    success: false,
                    output: None,
                    error: Some(
                        reply
                            .error
                            .map(|e| e.message)
                            .unwrap_or_else(|| "Calibration failed".into()),
                    ),
                },
                Err(_) => CalibrationApplied {
                    state: super::calibration::get().ok().and_then(|i| i.state),
                    success: false,
                    output: None,
                    error: Some(
                        "Optical owner unavailable; execution outcome may be unknown".into(),
                    ),
                },
            };
            serde_json::to_value(result).expect("calibration result is JSON serializable")
        }))
    }
    fn get_capabilities(&self, _: EmptyRequest) -> Result<Capabilities, i32> {
        Ok(crate::capabilities::compiled())
    }
    fn get_platform_info(&self, _: EmptyRequest) -> Result<PlatformInfoReply, i32> {
        Ok(self.optics.platform.info())
    }
    fn optical_status(&self, _: EmptyRequest) -> Result<OpticalReply, i32> {
        optical(&self.optics, OpticalCommand::Status)
    }
    fn optical_set(&self, request: OpticalSetRequest) -> Result<OpticalReply, i32> {
        optical(&self.optics, OpticalCommand::Set(request))
    }
    fn get_status(&self, _: EmptyRequest) -> Result<StatusReply, i32> {
        let provisioning = serde_json::from_value(crate::provisioning::snapshot())
            .map_err(|_| UBUS_ERROR_INTERNAL)?;
        let devices = self.devices.lock().map_err(|_| UBUS_ERROR_INTERNAL)?;
        let engine = self.engine.lock().map_err(|_| UBUS_ERROR_INTERNAL)?;
        let mut status = crate::platform::h3c_hm2004_du::status::snapshot(
            &engine,
            &self.services,
            (&devices, provisioning),
        );
        if let Some(jobs) = self.jobs.snapshot() {
            status.jobs = Some(serde_json::from_value(jobs).map_err(|_| UBUS_ERROR_INTERNAL)?);
        }
        status.ubus = metrics::snapshot();
        status.ipc = Some(super::ipc_server::metrics::snapshot());
        Ok(status)
    }
    fn get_sync(&self, _: EmptyRequest) -> Result<SyncStatusReply, i32> {
        serde_json::from_value(crate::provisioning::snapshot()).map_err(|_| UBUS_ERROR_INTERNAL)
    }
    fn config_check(&self, _: EmptyRequest) -> Result<ConfigCheckReply, i32> {
        serde_json::from_str(&crate::provisioning::control("{\"op\":\"check\"}"))
            .map_err(|_| UBUS_ERROR_INTERNAL)
    }
    fn config_reload(&self, _: EmptyRequest) -> Result<ConfigReloadReply, i32> {
        serde_json::from_str(&crate::provisioning::control("{\"op\":\"reload\"}"))
            .map_err(|_| UBUS_ERROR_INTERNAL)
    }
    fn sync(&self, request: SyncRequest) -> Result<SyncReply, i32> {
        let payload = request.provisioning_request().map_err(|_| 2)?;
        serde_json::from_str(&crate::provisioning::control(&payload.to_string()))
            .map_err(|_| UBUS_ERROR_INTERNAL)
    }
    fn restart_service(&self, _: EmptyRequest) -> Result<RestartReply, i32> {
        std::thread::Builder::new()
            .name("service-restart".into())
            .spawn(|| {
                // Let ubus deliver admission before the service exits. Never retry a write.
                std::thread::sleep(std::time::Duration::from_millis(500));
                // procd owns the helper: descendants of fttrd are killed with its
                // service cgroup, even if they use a separate process group.
                let result = openwrt_ubus::Ubus::connect(None).and_then(|mut bus| {
                    bus.call(
                        "service",
                        "event",
                        Some(&serde_json::json!({"type": "fttr.restart", "data": {}})),
                        5000,
                    )
                });
                if let Err(error) = result {
                    eprintln!("[ubus] service restart request failed: {error}");
                }
            })
            .map_err(|_| UBUS_ERROR_INTERNAL)?;
        Ok(RestartReply { restarting: true })
    }
    fn npu_status(&self, _: EmptyRequest) -> Result<NpuStatus, i32> {
        #[cfg(feature = "npu")]
        {
            Ok(crate::npu::status())
        }
        #[cfg(not(feature = "npu"))]
        {
            Err(4)
        }
    }
}

pub(super) fn methods(
    optics: OpticalClient,
    devices: std::sync::Arc<std::sync::Mutex<Vec<crate::ipc::SubDevice>>>,
    engine: std::sync::Arc<std::sync::Mutex<crate::pon_engine::PonEngine>>,
    services: crate::controller::service::States,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Vec<(String, openwrt_ubus::MethodHandler)> {
    let handlers = Handlers {
        optics,
        devices,
        engine,
        services,
        topology: Default::default(),
        jobs: {
            static JOBS: std::sync::OnceLock<fttr_jobs::Jobs> = std::sync::OnceLock::new();
            JOBS.get_or_init(|| fttr_jobs::Jobs::with_shutdown(stopping))
                .clone()
        },
    };
    macro_rules! register {
        ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {{
            let mut methods = Vec::new();
            $(if match $id.availability { Availability::Always => true, Availability::Npu => cfg!(feature = "npu"), Availability::Hl202 => cfg!(feature = "device-hl202") } {
                let handler = handlers.clone();
                methods.push(bind(&$id, move |request| handler.$wire(request)));
            })*
            methods
        }
        };
    }
    fttr_methods!(register)
}

fn response<R: DeserializeOwned>(value: serde_json::Value) -> Result<R, i32> {
    serde_json::from_value(value).map_err(|_| UBUS_ERROR_INTERNAL)
}

#[cfg(feature = "device-hl202")]
fn normalize_hl_result(
    query: &HlQuery,
    value: serde_json::Value,
) -> Result<serde_json::Value, i32> {
    match query {
        HlQuery::List => normalize::<HlListReply>(value),
        HlQuery::Report { .. } | HlQuery::SourceReport { .. } => normalize::<HlReportReply>(value),
        HlQuery::Run { request } => match &request.action {
            HlAction::Exec { .. } => normalize::<HlExecReply>(value),
            HlAction::AccountsRead
            | HlAction::AccountSetPassword { .. }
            | HlAction::AccountSetActive { .. } => normalize::<HlAccountReply>(value),
        },
    }
}

#[cfg(feature = "device-hl202")]
fn normalize<R: DeserializeOwned + Serialize>(
    value: serde_json::Value,
) -> Result<serde_json::Value, i32> {
    serde_json::to_value(response::<R>(value)?).map_err(|_| UBUS_ERROR_INTERNAL)
}
