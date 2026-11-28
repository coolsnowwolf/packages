//! Backend-neutral asynchronous command job. Device contracts live in their adapters.
use crate::provisioning::model::Device;
use serde_json::Value;
use std::future::Future;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[derive(Debug)]
pub struct Failure {
    pub reason: String,
    pub uncertain: bool,
}

impl Failure {
    pub fn uncertain(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            uncertain: true,
        }
    }
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            uncertain: false,
        }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

// A single owner consumes the terminal result once. The JoinHandle is retained,
// so cancellation/panic cannot leave an empty mailbox waiting forever.
pub struct Job {
    task: Option<tokio::task::JoinHandle<Result<Value, Failure>>>,
    apply: bool,
}

impl Job {
    #[cfg(test)]
    pub(crate) fn test_spawn(
        work: impl Future<Output = Result<Value, Failure>> + Send + 'static,
    ) -> Self {
        Self::spawn(true, Duration::from_secs(60), work)
    }
    pub fn take(&mut self) -> Option<Result<Value, Failure>> {
        let task = self.task.as_mut()?;
        match std::pin::Pin::new(task).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Pending => None,
            Poll::Ready(outcome) => {
                self.task = None;
                Some(outcome.unwrap_or_else(|_| {
                    Err(Failure {
                        reason: "command executor terminated; execution outcome not established"
                            .into(),
                        uncertain: self.apply,
                    })
                }))
            }
        }
    }

    fn spawn(
        apply: bool,
        deadline: Duration,
        work: impl Future<Output = Result<Value, Failure>> + Send + 'static,
    ) -> Self {
        struct WakeOnDrop;
        impl Drop for WakeOnDrop {
            fn drop(&mut self) {
                crate::provisioning::controller::wake();
            }
        }
        let task = crate::runtime::spawn(async move {
            let _wake = WakeOnDrop;
            tokio::time::timeout(deadline, work)
                .await
                .unwrap_or_else(|_| {
                    Err(Failure {
                        reason:
                            "command effect deadline exceeded; remote execution is not cancelled"
                                .into(),
                        uncertain: apply,
                    })
                })
        });
        Self {
            task: Some(task),
            apply,
        }
    }

    pub(super) fn observe(device: Device, reboot_from: Option<String>) -> Self {
        let auth_epoch = crate::omci_auth::serial_epoch(&device.optical_sn);
        Self::spawn(
            reboot_from.is_some(),
            Duration::from_secs(100),
            async move {
                #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
                if let Some(epoch) =
                    auth_epoch.filter(|_| device.provisioning_adapter == "hl202-mqtt")
                {
                    return crate::omci_auth::execution_scope(
                        device.optical_sn.clone(),
                        epoch,
                        async {
                            if let Some(boot_id) = reboot_from.as_deref() {
                                #[cfg(feature = "diagnostics")]
                                return crate::devices::hl202::provisioning::restart_for_recovery(
                                    &device, boot_id,
                                )
                                .await;
                                #[cfg(not(feature = "diagnostics"))]
                                {
                                    let _ = boot_id;
                                    return Err(
                                        "recovery restart requires the diagnostics build".into()
                                    );
                                }
                            }
                            crate::devices::hl202::provisioning::observe(&device, epoch).await
                        },
                    )
                    .await;
                }
                let _ = (device, auth_epoch, reboot_from);
                Err("HL202 recovery observation unavailable or unauthorized".into())
            },
        )
    }

    pub(super) fn recover(device: Device, apply: bool, operation_id: u64) -> Self {
        let auth_epoch = crate::omci_auth::serial_epoch(&device.optical_sn);
        Self::spawn(apply, Duration::from_secs(8 * 60), async move {
            #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
            if let Some(epoch) = auth_epoch.filter(|_| device.provisioning_adapter == "hl202-mqtt")
            {
                return crate::omci_auth::execution_scope(
                    device.optical_sn.clone(),
                    epoch,
                    crate::devices::hl202::provisioning::recover_pending(
                        device,
                        apply,
                        operation_id,
                        epoch,
                    ),
                )
                .await;
            }
            let _ = (device, apply, operation_id, auth_epoch);
            Err("HL202 transaction recovery unavailable or unauthorized".into())
        })
    }

    pub fn start(
        device: Device,
        apply: bool,
        full: bool,
        revision: String,
        operation_id: u64,
    ) -> Self {
        let auth_epoch = crate::omci_auth::serial_epoch(&device.optical_sn);
        Self::spawn(apply, Duration::from_secs(8 * 60), async move {
            #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
            let outcome = if auth_epoch.is_none() {
                Err("OMCI authentication pending".into())
            } else if device.provisioning_adapter == "hl202-mqtt" {
                crate::omci_auth::execution_scope(
                    device.optical_sn.clone(),
                    auth_epoch.unwrap(),
                    crate::devices::hl202::provisioning::run(
                        device,
                        apply,
                        full,
                        revision,
                        operation_id,
                        auth_epoch.unwrap(),
                    ),
                )
                .await
            } else {
                Err("command provisioning unsupported by this adapter".into())
            };
            #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
            let outcome = {
                let _ = (device, apply, full, revision, operation_id, auth_epoch);
                Err("command provisioning not compiled".into())
            };
            outcome
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn terminal(job: &mut Job) -> Result<Value, Failure> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(result) = job.take() {
                    break result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervised job must terminate")
    }

    #[tokio::test]
    async fn supervised_effects_have_one_terminal_result_on_success_panic_abort_and_deadline() {
        for apply in [false, true] {
            let mut job = Job::spawn(apply, Duration::from_secs(1), async {
                Ok(serde_json::json!({"ok":true}))
            });
            assert_eq!(terminal(&mut job).await.unwrap()["ok"], true);
            assert!(job.take().is_none());
            let mut job = Job::spawn(apply, Duration::from_secs(1), async {
                panic!("injected effect panic")
            });
            assert_eq!(terminal(&mut job).await.unwrap_err().uncertain, apply);
            assert!(job.take().is_none());
            let mut job = Job::spawn(apply, Duration::from_secs(1), std::future::pending());
            job.task.as_ref().unwrap().abort();
            assert_eq!(terminal(&mut job).await.unwrap_err().uncertain, apply);
            let mut job = Job::spawn(apply, Duration::from_millis(1), std::future::pending());
            assert_eq!(terminal(&mut job).await.unwrap_err().uncertain, apply);
            assert!(job.take().is_none());
        }
    }
}
