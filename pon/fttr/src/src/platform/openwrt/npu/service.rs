use super::reconciler::{self, Admission, Reconciler};
use crate::platform::adapter::PlatformId;
use fttr_api::NpuAdmissionModel;
use std::sync::{Arc, Mutex};

/// Map a hardware platform ID to its supported NPU admission model, if any.
pub fn admission_model_for(id: PlatformId) -> Option<NpuAdmissionModel> {
    match id {
        PlatformId::H3cHm2004Du => Some(NpuAdmissionModel::PortMask),
        PlatformId::FiberhomeAn4475G2 => Some(NpuAdmissionModel::DirectionOnly),
        PlatformId::TianyiTyfza1 => None,
    }
}

/// Active hardware platform's NPU admission model, if supported.
pub fn active_admission_model() -> Option<NpuAdmissionModel> {
    crate::platform::context::PlatformContext::detect()
        .ok()
        .and_then(|platform| admission_model_for(platform.id()))
}

/// Check if current hardware platform supports Airoha NPU offload.
pub fn is_supported_platform() -> bool {
    active_admission_model().is_some()
}

/// Non-owning handle used by subsystems (such as optical quiesce) to withdraw
/// kernel offload admission without permanently stopping the reconciler.
#[derive(Clone)]
pub struct ServiceHandle(Arc<Mutex<Admission>>);

impl ServiceHandle {
    pub fn withdraw(&self) {
        let mut admission = self.0.lock().unwrap();
        if let Err(error) = admission.withdraw() {
            eprintln!("[-] NPU quiesce withdrawal failed: {error}");
        }
    }
}

/// The daemon owns this guard; stopping and publishing admission share one lock.
/// A poll suspended in a command cannot re-arm admission after stop().
pub struct Service(pub(crate) Arc<Mutex<Admission>>);

impl Service {
    pub fn handle(&self) -> ServiceHandle {
        ServiceHandle(self.0.clone())
    }

    pub(crate) fn start(platform: crate::platform::context::PlatformContext) -> Self {
        let model = admission_model_for(platform.id())
            .expect("NPU service started on unsupported platform");
        let admission = Arc::new(Mutex::new(Admission::for_model(model)));
        let service = Self(admission.clone());
        // Revoke stale admission before discovery, including after daemon restart.
        if let Err(error) = admission.lock().unwrap().withdraw() {
            eprintln!("[-] NPU initial withdrawal failed: {error}");
        }
        crate::runtime::spawn_service("NPU admission", async move {
            let _guard = Service(admission.clone());
            let mut reconciler = Reconciler::default();
            let mut last_error = None;
            let mut poll_interval = reconciler::POLL_FAST;
            loop {
                tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    _ = tokio::time::sleep(poll_interval) => (),
                }
                let result = tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    result = reconciler::reconcile(&admission, &mut reconciler) => result,
                };
                match result {
                    Ok(next_interval) => {
                        last_error = None;
                        poll_interval = next_interval;
                    }
                    Err(error) => {
                        poll_interval = reconciler::POLL_FAST;
                        reconciler = Reconciler::default();
                        if let Err(error) = admission.lock().unwrap().withdraw() {
                            eprintln!("[-] NPU withdrawal failed: {error}");
                        }
                        let message = error.to_string();
                        if last_error.as_ref() != Some(&message) {
                            eprintln!("[-] NPU admission paused: {message}");
                            last_error = Some(message);
                        }
                    }
                }
            }
        });
        service
    }

    pub fn stop(&self) {
        let mut admission = self.0.lock().unwrap();
        admission.stopped = true;
        if let Err(error) = admission.withdraw() {
            eprintln!("[-] NPU shutdown withdrawal failed: {error}");
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stopped_service_cannot_be_rearmed_by_an_inflight_poll() {
        let path = std::env::temp_dir().join(format!("fttr-npu-{}", std::process::id()));
        let dir_path = std::env::temp_dir().join(format!("fttr-npu-dir-{}", std::process::id()));
        fs::write(&path, "3\n").unwrap();
        fs::write(&dir_path, "7\n").unwrap();
        let service = Service(Arc::new(Mutex::new(Admission {
            model: NpuAdmissionModel::PortMask,
            path: path.clone(),
            directions_path: dir_path.clone(),
            stopped: false,
        })));
        service.stop();
        service.0.lock().unwrap().set(3).unwrap();
        assert_eq!(reconciler::text(&path).unwrap(), "0");
        assert_eq!(reconciler::text(&dir_path).unwrap(), "0");
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(dir_path);
    }

    #[test]
    fn direction_only_stopped_service_stays_withdrawn() {
        let dir_path =
            std::env::temp_dir().join(format!("fttr-npu-dironly-{}", std::process::id()));
        fs::write(&dir_path, "3\n").unwrap();
        let service = Service(Arc::new(Mutex::new(Admission::new_direction_only(
            dir_path.clone(),
        ))));
        service.stop();
        service.0.lock().unwrap().set_directions(3).unwrap();
        assert_eq!(reconciler::text(&dir_path).unwrap(), "0");
        let _ = fs::remove_file(dir_path);
    }

    #[test]
    fn admission_model_mapping_matches_platforms() {
        assert_eq!(
            admission_model_for(PlatformId::H3cHm2004Du),
            Some(NpuAdmissionModel::PortMask)
        );
        assert_eq!(
            admission_model_for(PlatformId::FiberhomeAn4475G2),
            Some(NpuAdmissionModel::DirectionOnly)
        );
    }
}
