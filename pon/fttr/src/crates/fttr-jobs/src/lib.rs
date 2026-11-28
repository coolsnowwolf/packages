//! Bounded in-memory diagnostics, independent of ubus connections.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};

const CHUNK: usize = 32768;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const TOTAL_BYTES: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct Jobs(Arc<Mutex<State>>);
struct State {
    jobs: BTreeMap<String, Job>,
    running: usize,
    sequence: u64,
    epoch: u128,
    stopping: Arc<AtomicBool>,
}
struct Job {
    kind: &'static str,
    created: Instant,
    result: Option<Vec<u8>>,
    released: bool,
    progress: Option<Value>,
}

#[derive(Clone, Copy)]
pub enum Phase {
    Starting,
    Upload,
    Tables,
}

/// A weak handle cannot extend the lifetime of the job store or retain results.
#[derive(Clone)]
pub struct Progress {
    state: Weak<Mutex<State>>,
    id: String,
}
impl Progress {
    pub fn update(&self, phase: Phase, received: usize, expected: Option<usize>) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.try_lock() else {
            return;
        };
        if let Some(job) = state.jobs.get_mut(&self.id) {
            if !job.released && job.created.elapsed() < TTL && job.result.is_none() {
                let phase = match phase {
                    Phase::Starting => "starting",
                    Phase::Upload => "upload",
                    Phase::Tables => "tables",
                };
                job.progress = Some(json!({"phase":phase,"received":received,"expected":expected}));
            }
        }
    }
}

fn sweep(state: &mut State) {
    state
        .jobs
        .retain(|_, job| job.result.is_none() || (!job.released && job.created.elapsed() < TTL));
}

pub fn error(message: &str) -> Value {
    json!({"error":message})
}

impl Default for Jobs {
    fn default() -> Self {
        Self::with_shutdown(Arc::new(AtomicBool::new(false)))
    }
}

impl Jobs {
    pub fn snapshot(&self) -> Option<Value> {
        let state = self.0.try_lock().ok()?;
        Some(json!({
            "accepting": !state.stopping.load(Ordering::Acquire),
            "running": state.running,
            "stored": state.jobs.len(),
            "bytes": state.jobs.values().filter_map(|job|job.result.as_ref()).map(Vec::len).sum::<usize>(),
            "running_limit": 4, "stored_limit": 32,
            "bytes_limit": TOTAL_BYTES, "result_bytes_limit": MAX_BYTES,
        }))
    }

    pub fn with_shutdown(stopping: Arc<AtomicBool>) -> Self {
        Self::with_sweep(stopping, Duration::from_secs(30))
    }

    fn with_sweep(stopping: Arc<AtomicBool>, interval: Duration) -> Self {
        let jobs = Self(Arc::new(Mutex::new(State {
            jobs: BTreeMap::new(),
            running: 0,
            sequence: 0,
            epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            stopping,
        })));
        let weak = Arc::downgrade(&jobs.0);
        std::thread::Builder::new()
            .name("fttr-job-expiry".into())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let Some(state) = weak.upgrade() else { break };
                if let Ok(mut state) = state.lock() {
                    sweep(&mut state);
                };
            })
            .expect("cannot start job expiry worker");
        jobs
    }
    pub fn start(
        &self,
        kind: &'static str,
        work: impl FnOnce() -> Value + Send + 'static,
    ) -> Value {
        self.start_with_progress(kind, move |_| work())
    }

    pub fn start_with_progress(
        &self,
        kind: &'static str,
        work: impl FnOnce(Progress) -> Value + Send + 'static,
    ) -> Value {
        let Ok(mut state) = self.0.try_lock() else {
            return error("Job queue busy");
        };
        if state.stopping.load(Ordering::Acquire) {
            return error("Service is stopping; job was not admitted");
        }
        sweep(&mut state);
        if state.running >= 4 || state.jobs.len() >= 32 {
            return error("Job capacity reached");
        }
        state.sequence += 1;
        let id = format!("{:x}-{:x}", state.epoch, state.sequence);
        state.jobs.insert(
            id.clone(),
            Job {
                kind,
                created: Instant::now(),
                result: None,
                released: false,
                progress: None,
            },
        );
        state.running += 1;
        drop(state);
        let jobs = self.clone();
        let worker_id = id.clone();
        let progress = Progress {
            state: Arc::downgrade(&self.0),
            id: id.clone(),
        };
        let spawn = std::thread::Builder::new()
            .name(format!("fttr-{kind}"))
            .spawn(move || {
                let value =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(progress)))
                        .unwrap_or_else(|_| error("Worker stopped; execution outcome is unknown"));
                let mut output = Limited(Vec::new());
                if serde_json::to_writer(&mut output, &value).is_err() {
                    output.0 = error("Job result exceeds transport limit")
                        .to_string()
                        .into_bytes();
                }
                if let Ok(mut state) = jobs.0.lock() {
                    state.running -= 1;
                    let total: usize = state
                        .jobs
                        .values()
                        .filter_map(|job| job.result.as_ref())
                        .map(Vec::len)
                        .sum();
                    // Reserve space for bounded error envelopes for every slot.
                    if total + output.0.len() > TOTAL_BYTES - 4096 {
                        output.0 = error("Job result storage full").to_string().into_bytes();
                    }
                    if let Some(job) = state.jobs.get_mut(&worker_id) {
                        job.result = Some(output.0);
                    }
                    sweep(&mut state);
                }
            });
        if spawn.is_err() {
            if let Ok(mut state) = self.0.lock() {
                state.jobs.remove(&id);
                state.running -= 1;
            }
            return error("Cannot start job worker");
        }
        json!({"status":"pending","job":id})
    }

    pub fn result(&self, id: &str, chunk: u16, kind: Option<&str>) -> Value {
        // These critical sections only inspect or update bounded in-memory data.
        // Contention with completion/expiry is not a fatal polling error.
        let Ok(mut state) = self.0.lock() else {
            return error("Job store unavailable; execution outcome is unknown");
        };
        let Some(job) = state.jobs.get(id) else {
            // The daemon instance is part of the opaque ID. Losing a store is
            // not evidence that an admitted write did not execute.
            if id.len() <= 128 {
                if let Some((epoch, sequence)) = id.split_once('-') {
                    if u128::from_str_radix(epoch, 16).is_ok_and(|epoch| epoch != state.epoch)
                        && u64::from_str_radix(sequence, 16).is_ok()
                    {
                        return error("Job belongs to a previous service instance; execution outcome is unknown");
                    }
                }
            }
            return error("Job expired");
        };
        if job.released || job.created.elapsed() >= TTL {
            if job.result.is_some() {
                state.jobs.remove(id);
            }
            return error("Job expired");
        }
        if kind.is_some_and(|kind| kind != job.kind) {
            return error("Invalid job type");
        }
        let Some(bytes) = &job.result else {
            let mut pending = json!({"status":"pending"});
            if let Some(progress) = &job.progress {
                pending["progress"] = progress.clone();
            }
            return pending;
        };
        let chunks = bytes.len().div_ceil(CHUNK);
        let offset = usize::from(chunk) * CHUNK;
        if offset >= bytes.len() {
            return error("Invalid result chunk");
        }
        json!({"status":"ready","chunk":chunk,"chunks":chunks,"bytes":bytes.len(),"data":STANDARD.encode(&bytes[offset..bytes.len().min(offset + CHUNK)])})
    }

    // Release discards results; it does not retry or cancel side effects.
    pub fn release(&self, id: &str) -> Value {
        let Ok(mut state) = self.0.lock() else {
            return error("Job store unavailable; execution outcome is unknown");
        };
        if let Some(job) = state.jobs.get_mut(id) {
            job.released = true;
            if job.result.is_some() {
                state.jobs.remove(id);
            }
        }
        json!({"released":true})
    }
}

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_BYTES {
            return Err(io::Error::other("result limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn polling_and_release_wait_for_short_store_contention() {
        let jobs = Jobs::default();
        jobs.0.lock().unwrap().jobs.insert(
            "completed".into(),
            Job {
                kind: "test",
                created: Instant::now(),
                result: Some(br#"{"done":true}"#.to_vec()),
                released: false,
                progress: None,
            },
        );
        for release in [false, true] {
            let guard = jobs.0.lock().unwrap();
            let worker_jobs = jobs.clone();
            let (started, ready) = std::sync::mpsc::channel();
            let (sent, received) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                started.send(()).unwrap();
                sent.send(if release {
                    worker_jobs.release("completed")
                } else {
                    worker_jobs.result("completed", 0, None)
                })
                .unwrap();
            });
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
            drop(guard);
            let reply = received.recv_timeout(Duration::from_secs(2)).unwrap();
            if release {
                assert_eq!(reply, json!({"released":true}));
            } else {
                assert_eq!(reply["status"], "ready");
                assert_eq!(
                    STANDARD.decode(reply["data"].as_str().unwrap()).unwrap(),
                    br#"{"done":true}"#
                );
            }
            worker.join().unwrap();
        }
        assert!(jobs.0.lock().unwrap().jobs.is_empty());
    }
    #[test]
    fn daemon_restart_does_not_report_missing_admitted_work_as_not_executed() {
        let first = Jobs::default();
        let id = first.start("write", || json!({"done":true}))["job"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = Jobs::default();
        assert!(second.result(&id, 0, None)["error"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown"));
        assert_eq!(second.snapshot().unwrap()["running"], 0);
    }
    #[test]
    fn progress_shutdown_and_idle_expiry_do_not_cancel_inflight_work() {
        let stopping = Arc::new(AtomicBool::new(false));
        let jobs = Jobs::with_sweep(stopping.clone(), Duration::from_millis(5));
        let (sent, received) = std::sync::mpsc::channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let id = jobs.start_with_progress("mib", move |progress| {
            progress.update(Phase::Upload, 7, Some(20));
            sent.send(()).unwrap();
            worker_barrier.wait();
            json!({"done":true})
        })["job"]
            .as_str()
            .unwrap()
            .to_owned();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            jobs.result(&id, 0, None)["progress"],
            json!({"phase":"upload","received":7,"expected":20})
        );
        stopping.store(true, Ordering::Release);
        assert!(jobs
            .start("test", || panic!("must not be admitted"))
            .get("error")
            .is_some());
        jobs.0.lock().unwrap().jobs.get_mut(&id).unwrap().created = Instant::now() - TTL;
        std::thread::sleep(Duration::from_millis(20));
        // Expired running work still consumes its slot until it exits.
        assert_eq!(jobs.0.lock().unwrap().running, 1);
        let stats = jobs.snapshot().unwrap();
        assert_eq!(stats["running"], 1);
        assert_eq!(stats["accepting"], false);
        barrier.wait();
        for _ in 0..1000 {
            if jobs.0.lock().unwrap().running == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!jobs.0.lock().unwrap().jobs.contains_key(&id));
        jobs.0.lock().unwrap().jobs.insert(
            "idle".into(),
            Job {
                kind: "test",
                created: Instant::now() - TTL,
                result: Some(vec![0]),
                released: false,
                progress: None,
            },
        );
        // No admission, result polling or release call is needed for cleanup.
        for _ in 0..1000 {
            if jobs.0.lock().unwrap().jobs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(jobs.0.lock().unwrap().jobs.is_empty());
    }
    #[test]
    fn chunks_are_stable_and_worker_panic_is_not_success() {
        let jobs = Jobs::default();
        let value = json!({"text":"x".repeat(CHUNK+50)});
        let expected = value.clone();
        let id = jobs.start("test", move || value)["job"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 0..1000 {
            if jobs.result(&id, 0, None)["status"] == "ready" {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let first = jobs.result(&id, 0, None);
        let second = jobs.result(&id, 1, None);
        assert_eq!(first["chunks"], 2);
        assert_eq!(first["bytes"], second["bytes"]);
        let mut bytes = STANDARD.decode(first["data"].as_str().unwrap()).unwrap();
        bytes.extend(STANDARD.decode(second["data"].as_str().unwrap()).unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
        let id = jobs.start("test", || panic!("injected worker failure"))["job"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 0..1000 {
            if jobs.result(&id, 0, None)["status"] == "ready" {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let reply = jobs.result(&id, 0, None);
        let result: Value =
            serde_json::from_slice(&STANDARD.decode(reply["data"].as_str().unwrap()).unwrap())
                .unwrap();
        assert!(result["error"].as_str().unwrap().contains("unknown"));
    }
    #[test]
    fn admission_is_bounded_and_release_does_not_cancel_workers() {
        let jobs = Jobs::default();
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut ids = Vec::new();
        for _ in 0..4 {
            let barrier = barrier.clone();
            ids.push(
                jobs.start("test", move || {
                    barrier.wait();
                    json!({"done":true})
                })["job"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert!(jobs.start("test", || Value::Null).get("error").is_some());
        assert_eq!(jobs.result(&ids[0], 0, None)["status"], "pending");
        jobs.release(&ids[0]);
        barrier.wait();
        for _ in 0..100 {
            if jobs.0.lock().unwrap().running == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(jobs.0.lock().unwrap().running, 0);
        assert!(jobs.result(&ids[0], 0, None).get("error").is_some());
        assert!(jobs
            .result(&ids[1], 0, Some("other"))
            .get("error")
            .is_some());
        let result = jobs.result(&ids[1], 0, None);
        let decoded = STANDARD.decode(result["data"].as_str().unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&decoded).unwrap(),
            json!({"done":true})
        );
        assert!(jobs.result(&ids[1], 1, None).get("error").is_some());
    }
    #[test]
    fn result_size_and_expiry_are_enforced() {
        let mut limited = Limited(Vec::new());
        assert!(limited.write_all(&vec![0; MAX_BYTES + 1]).is_err());
        let jobs = Jobs::default();
        jobs.0.lock().unwrap().jobs.insert(
            "expired".into(),
            Job {
                kind: "test",
                created: Instant::now() - TTL,
                result: Some(vec![0]),
                released: false,
                progress: None,
            },
        );
        assert!(jobs.result("expired", 0, None).get("error").is_some());
        assert!(jobs.0.lock().unwrap().jobs.is_empty());
    }
}
