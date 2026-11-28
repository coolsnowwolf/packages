use super::connection_request_http::{self, Outcome, Target};
use super::{init, now, Server, TaskState};
use crate::provisioning::model::ConnectionRequest;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

const COOLDOWN: Duration = Duration::from_secs(30);
const ACCEPTED_WAIT: Duration = Duration::from_secs(5);
const MAX_INFLIGHT: usize = 8;
static START: Once = Once::new();
static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub(super) fn wake() {
    WAKE.notify_one();
}

#[derive(Clone, Serialize)]
pub(super) struct Status {
    attempted_at: u64,
    finished_at: Option<u64>,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Default)]
pub(super) struct Worker {
    attempts: BTreeMap<String, (Instant, u64)>,
    inflight: BTreeSet<String>,
    jobs: tokio::task::JoinSet<String>,
}

impl Worker {
    #[cfg(test)]
    pub(super) fn poll(
        &mut self,
        server: &Arc<Server>,
        configs: &BTreeMap<String, ConnectionRequest>,
        at: Instant,
    ) {
        crate::runtime::block_on(async {
            self.poll_async(server, configs, at).await;
            while let Some(result) = self.jobs.join_next().await {
                if let Ok(id) = result {
                    self.inflight.remove(&id);
                }
            }
        });
    }

    pub(super) async fn poll_async(
        &mut self,
        server: &Arc<Server>,
        configs: &BTreeMap<String, ConnectionRequest>,
        at: Instant,
    ) {
        if !server.enabled || server.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        while let Some(result) = self.jobs.try_join_next() {
            if let Ok(id) = result {
                self.inflight.remove(&id);
            }
        }
        if self.jobs.is_empty() {
            self.inflight.clear();
        }
        while self.jobs.len() < MAX_INFLIGHT {
            let job = {
                let mut state = server.state.lock().unwrap();
                if state.load_failure.is_some() {
                    return;
                }
                state.expire();
                self.attempts.retain(|id, _| state.devices.contains_key(id));
                state
                    .connection_requests
                    .retain(|id, _| configs.get(id).is_some_and(|c| c.enabled));
                let candidates: Vec<_> = configs
                    .iter()
                    .filter(|(id, config)| {
                        config.enabled
                            && !self.inflight.contains(*id)
                            && state.devices.contains_key(*id)
                            && self.attempts.get(*id).is_none_or(|(last, queued)| {
                                let accepted = state
                                    .connection_requests
                                    .get(*id)
                                    .is_some_and(|s| matches!(s.outcome, Outcome::Accepted { .. }));
                                let next_batch = accepted
                                    && state
                                        .tasks
                                        .values()
                                        .find(|t| t.device == **id && t.state == TaskState::Queued)
                                        .is_some_and(|t| t.id != *queued);
                                next_batch
                                    || at.duration_since(*last)
                                        >= if accepted { ACCEPTED_WAIT } else { COOLDOWN }
                            })
                            && state
                                .tasks
                                .values()
                                .any(|t| t.device == **id && t.state == TaskState::Queued)
                            && !state
                                .tasks
                                .values()
                                .any(|t| t.device == **id && t.state == TaskState::Sent)
                    })
                    .collect();
                for (id, _) in &candidates {
                    state.dispatch_demand(id);
                }
                let candidate = candidates
                    .into_iter()
                    .filter(|(id, _)| state.schedule_device(id))
                    .min_by_key(|(id, _)| self.attempts.get(*id).copied());
                let Some((id, config)) = candidate else {
                    return;
                };
                let device = &state.devices[id];
                let target = device.inform["parameters"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|p| {
                        matches!(
                            p["name"].as_str(),
                            Some(
                                "InternetGatewayDevice.ManagementServer.ConnectionRequestURL"
                                    | "Device.ManagementServer.ConnectionRequestURL"
                            )
                        )
                    })
                    .and_then(|p| p["value"].as_str())
                    .ok_or(Outcome::InvalidUrl)
                    .and_then(|url| Target::parse(url, &device.peer));
                let queued = state
                    .tasks
                    .values()
                    .find(|t| t.device == *id && t.state == TaskState::Queued)
                    .unwrap()
                    .id;
                self.attempts.insert(id.clone(), (at, queued));
                if let Some(proof) =
                    super::connection_request_proof::Proof::new(&state.devices[id], config)
                {
                    state.connection_request_proofs.insert(id.clone(), proof);
                } else {
                    state.connection_request_proofs.remove(id);
                }
                state.connection_requests.insert(
                    id.clone(),
                    Status {
                        attempted_at: now(),
                        finished_at: None,
                        outcome: Outcome::Requesting,
                    },
                );
                (id.clone(), config.clone(), target)
            };
            let (id, credentials, target) = job;
            self.inflight.insert(id.clone());
            let server = server.clone();
            self.jobs.spawn(async move {
                let outcome = match target {
                    Ok(target) => {
                        connection_request_http::request_async(&target, &credentials).await
                    }
                    Err(outcome) => outcome,
                };
                let mut state = server.state.lock().unwrap();
                if let Some(proof) = state.connection_request_proofs.get_mut(&id) {
                    proof.authenticated(matches!(outcome, Outcome::Accepted { .. }));
                }
                if let Some(status) = state.connection_requests.get_mut(&id) {
                    status.finished_at = Some(now());
                    status.outcome = outcome;
                }
                id
            });
        }
    }
}

pub(super) fn start() {
    START.call_once(|| {
        crate::runtime::spawn_service("ACS connection requests", async {
            let mut worker = Worker::default();
            while !crate::runtime::is_stopping() {
                let Ok((server, configs)) = crate::runtime::spawn_blocking(|| {
                    let configs = crate::provisioning::connection_request_devices();
                    let server = init();
                    super::periodic_policy::poll(&server, &configs);
                    (server, configs)
                })
                .await
                else {
                    break;
                };
                worker.poll_async(&server, &configs, Instant::now()).await;
                tokio::select! {
                    _ = WAKE.notified() => (),
                    _ = tokio::time::sleep(Duration::from_millis(250)) => (),
                }
            }
        });
    });
}
