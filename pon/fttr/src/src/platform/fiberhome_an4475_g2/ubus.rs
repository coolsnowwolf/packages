//! AN4475-G2 ubus handlers driven by shared `fttr_api` wire contracts.
use super::runtime;
use crate::{
    controller::{
        optical_owner::{OpticalClient, OpticalCommand, OpticalReplyExt},
        service,
        ubus_mib::UBUS_ERROR_INTERNAL,
    },
    ipc::SubDevice,
    pon_engine::PonEngine,
};
use fttr_api::*;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
#[path = "../ubus_topology.rs"]
mod topology;

fn bind<Q: DeserializeOwned + 'static, R: Serialize + 'static>(
    method: &Method<Q, R>,
    handler: impl Fn(Q) -> Result<R, i32> + Send + Sync + 'static,
) -> (String, openwrt_ubus::MethodHandler) {
    // Handlers run synchronously on the ubus server thread; long operations
    // (optical transitions up to 15s, MIB jobs) belong on the optical actor or
    // the job system. A stuck handler delays other ubus callers — there is no
    // per-call timeout wrapper, so keep handlers bounded by construction.
    (
        method.name.into(),
        Box::new(move |_, message| {
            let mut payload = message.cloned().unwrap_or_else(|| serde_json::json!({}));
            if let Some(object) = payload.as_object_mut() {
                object.remove("ubus_rpc_session");
            }
            let request = serde_json::from_value(payload).map_err(|_| 2)?;
            serde_json::to_value(handler(request)?).map_err(|_| UBUS_ERROR_INTERNAL)
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

fn response<R: DeserializeOwned>(value: serde_json::Value) -> Result<R, i32> {
    serde_json::from_value(value).map_err(|_| UBUS_ERROR_INTERNAL)
}

#[derive(Clone)]
struct Handlers {
    optics: OpticalClient,
    devices: Arc<Mutex<Vec<SubDevice>>>,
    engine: Arc<Mutex<PonEngine>>,
    services: service::States,
    jobs: fttr_jobs::Jobs,
    topology: Arc<Mutex<crate::controller::topology::Cache>>,
}

impl Handlers {
    fn get_capabilities(&self, _: EmptyRequest) -> Result<Capabilities, i32> {
        Ok(crate::capabilities::compiled())
    }
    fn get_platform_info(&self, _: EmptyRequest) -> Result<PlatformInfoReply, i32> {
        Ok(self.optics.platform.info())
    }
    fn fpga_diagnostic(&self, _: FpgaDiagnosticRequest) -> Result<FpgaDiagnosticReply, i32> {
        Ok(FpgaDiagnosticReply::Error {
            error: "FPGA diagnostic unsupported on AN4475-G2".into(),
        })
    }
    fn optical_status(&self, _: EmptyRequest) -> Result<OpticalReply, i32> {
        optical(&self.optics, OpticalCommand::Status)
    }
    fn optical_set(&self, request: OpticalSetRequest) -> Result<OpticalReply, i32> {
        optical(&self.optics, OpticalCommand::Set(request))
    }
    fn get_status(&self, _: EmptyRequest) -> Result<StatusReply, i32> {
        let provisioning =
            serde_json::from_value::<SyncStatusReply>(crate::provisioning::snapshot())
                .map_err(|_| UBUS_ERROR_INTERNAL)?;
        let mut status = runtime::status_snapshot(
            &self.optics,
            &self.engine,
            &self.devices,
            &self.services,
            provisioning,
        )?;
        if let Some(jobs) = self.jobs.snapshot() {
            status.jobs = serde_json::from_value(jobs).ok();
        }
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
                std::thread::sleep(Duration::from_millis(500));
                let _ = openwrt_ubus::Ubus::connect(None).and_then(|mut bus| {
                    bus.call(
                        "service",
                        "event",
                        Some(&serde_json::json!({"type": "fttr.restart", "data": {}})),
                        5000,
                    )
                });
            })
            .map_err(|_| UBUS_ERROR_INTERNAL)?;
        Ok(RestartReply { restarting: true })
    }
    fn npu_status(&self, _: EmptyRequest) -> Result<NpuStatus, i32> {
        #[cfg(feature = "npu")]
        {
            Ok(crate::npu::status_for_platform(
                crate::platform::adapter::PlatformId::FiberhomeAn4475G2,
            ))
        }
        #[cfg(not(feature = "npu"))]
        {
            Err(4)
        }
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
                crate::controller::hl202::control_query(
                    query.clone(),
                    &handler.engine,
                    &handler.services,
                )
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
        Ok(CalibrationInfoReply::Native(super::calibration::info()))
    }
    fn select_calibration(&self, _: CalibrationSelectRequest) -> Result<CalibrationInfoReply, i32> {
        Ok(CalibrationInfoReply::Error(CommandError {
            error: "calibration selection unsupported on AN4475-G2".into(),
        }))
    }
    fn save_calibration(&self, _: CalibrationSaveRequest) -> Result<CalibrationSaveReply, i32> {
        Ok(CalibrationSaveReply::Error(CommandError {
            error: "calibration upload unsupported on AN4475-G2".into(),
        }))
    }
    fn apply_calibration(&self, _: EmptyRequest) -> Result<JobStartReply, i32> {
        let optics = self.optics.clone();
        response(self.jobs.start("calibration", move || {
            let reply = optical(&optics, OpticalCommand::Reinitialize);
            serde_json::to_value(CalibrationApplied {
                state: None,
                success: reply
                    .as_ref()
                    .is_ok_and(|r| r.status == ReplyStatus::Success),
                output: None,
                error: match reply {
                    Ok(ref value) if value.status != ReplyStatus::Success => Some(
                        value
                            .error
                            .as_ref()
                            .map(|e| e.message.clone())
                            .unwrap_or_else(|| "Calibration failed".into()),
                    ),
                    Err(code) => Some(format!("ubus error {code}")),
                    _ => None,
                },
            })
            .expect("calibration result is JSON serializable")
        }))
    }
}

pub(super) fn start(
    optics: OpticalClient,
    devices: Arc<Mutex<Vec<SubDevice>>>,
    engine: Arc<Mutex<PonEngine>>,
    services: service::States,
    stopping: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let handlers = Handlers {
        optics,
        devices,
        engine,
        services,
        topology: Default::default(),
        jobs: {
            static JOBS: std::sync::OnceLock<fttr_jobs::Jobs> = std::sync::OnceLock::new();
            JOBS.get_or_init(|| fttr_jobs::Jobs::with_shutdown(stopping.clone()))
                .clone()
        },
    };
    std::thread::Builder::new()
        .name("ubus-server".into())
        .spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut server = match openwrt_ubus::UbusServer::new(None) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[-] Native ubus server connect failed: {e}; retrying in 3s");
                        std::thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                };
                macro_rules! register {
                    ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {{
                        let mut methods = Vec::new();
                        $(if match $id.availability {
                            Availability::Always => true,
                            Availability::Npu => cfg!(feature = "npu"),
                            Availability::Hl202 => cfg!(feature = "device-hl202"),
                        } {
                            let handler = handlers.clone();
                            methods.push(bind(&$id, move |request| handler.$wire(request)));
                        })*
                        methods
                    }};
                }
                let methods = fttr_methods!(register);
                if let Err(e) = server.add_object(fttr_api::OBJECT, methods) {
                    eprintln!("[-] Failed to register 'fttr' ubus object: {e}; retrying in 3s");
                    std::thread::sleep(Duration::from_secs(3));
                    continue;
                }
                println!("[+] Registered native ubus object 'fttr'.");
                if let Err(e) = server.run_loop() {
                    eprintln!("[-] ubus server run_loop exited: {e}");
                }
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .ok()
}
