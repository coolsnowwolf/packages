//! One bounded, atomic recovery file. Device frames preserve their own intent.
use super::*;
use crate::provisioning::storage;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

const DIRECTORY: &str = "/tmp/fttr/state/recovery";
const MAX_DEVICES: usize = 256; // Includes retained orphan transactions.
const MAX_BYTES: usize = 128 * 1024;
const LEGACY_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadRecovery {
    pub attempts: u32,
    pub retry_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    device: String,
    serial: String,
    cwmp_id: String,
    operation: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    owned: BTreeMap<String, wan::Owned>,
    #[serde(default, skip_serializing_if = "is_default")]
    attempts: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    retry_at: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    reasons: scheduler::Reasons,
    #[serde(default, skip_serializing_if = "is_default")]
    read: ReadRecovery,
}
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    devices: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryFile {
    schema: u32,
    #[serde(deserialize_with = "unique_frames")]
    devices: BTreeMap<String, Snapshot>,
}
#[derive(Serialize)]
struct RecoveryView<'a> {
    schema: u32,
    devices: &'a BTreeMap<String, Snapshot>,
}
fn unique_frames<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Snapshot>, D::Error> {
    struct Frames;
    impl<'de> serde::de::Visitor<'de> for Frames {
        type Value = BTreeMap<String, Snapshot>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique bounded device frames")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut frames = BTreeMap::new();
            while let Some((id, frame)) = map.next_entry::<String, Snapshot>()? {
                if frames.len() >= MAX_DEVICES || frames.insert(id, frame).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive recovery devices",
                    ));
                }
            }
            Ok(frames)
        }
    }
    deserializer.deserialize_map(Frames)
}

// Compare a serialization against the durable acknowledgement without allocating
// another JSON buffer on every idle tick. Also bounds changed serializations.
struct Compare<'a> {
    acknowledged: &'a [u8],
    length: usize,
    equal: bool,
    limit: usize,
}
impl Write for Compare<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.length.checked_add(bytes.len()).ok_or_else(invalid)?;
        if end > self.limit {
            return Err(io::Error::other("recovery state exceeds size limit"));
        }
        self.equal &= self.acknowledged.get(self.length..end) == Some(bytes);
        self.length = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn check_frame(frame: &Snapshot) -> io::Result<()> {
    frame.validate()?;
    serde_json::to_writer(
        Compare {
            acknowledged: &[],
            length: 0,
            equal: false,
            limit: MAX_BYTES,
        },
        frame,
    )?;
    Ok(())
}

pub(super) struct Store {
    directory: String,
    frames: BTreeMap<String, Snapshot>,
    acknowledged: Vec<u8>,
    scratch: Vec<u8>,
    legacy_pending: bool,
    error: Option<String>,
    load_error: bool,
}

fn read_bounded(path: &str, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if path.starts_with(crate::provisioning::runtime_store::PREFIX) {
        bytes = storage::read(path)?;
    } else {
        use std::io::Read;
        std::fs::File::open(path)?
            .take((max + 1) as u64)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() > max {
        return Err(io::Error::other("recovery state exceeds size limit"));
    }
    Ok(bytes)
}
fn invalid() -> io::Error {
    io::Error::other("invalid recovery schema, identity, or budget")
}
fn intent_view(view: &Value) -> Value {
    let mut intent = serde_json::Map::new();
    for key in [
        "device",
        "optical_sn",
        "cwmp_id",
        "operation_id",
        "mode",
        "state",
        "revision",
        "unresolved_operation",
    ] {
        if let Some(value) = view.get(key) {
            intent.insert(key.into(), value.clone());
        }
    }
    intent.insert("effect_uncertain".into(), Value::Bool(true));
    intent.insert(
        "items".into(),
        Value::Array(
            view["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| {
                    let mut minimal = serde_json::Map::new();
                    for key in ["id", "kind", "state", "reason"] {
                        if let Some(value) = item.get(key) {
                            minimal.insert(key.into(), value.clone());
                        }
                    }
                    Value::Object(minimal)
                })
                .collect(),
        ),
    );
    Value::Object(intent)
}
impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || self.device.is_empty()
            || self.serial.is_empty()
            || self.attempts > 5
            || self.read.attempts > 3
            || self.reasons.0 & !127 != 0
        {
            return Err(invalid());
        }
        if let Some(op) = &self.operation {
            if op["device"] != self.device
                || op["optical_sn"] != self.serial
                || op["operation_id"].as_u64().is_none()
                || !matches!(op["mode"].as_str(), Some("plan" | "apply"))
                || OperationPhase::deserialize(&op["state"]).is_err()
                || !op["items"].is_array()
            {
                return Err(invalid());
            }
            for item in op["items"].as_array().unwrap() {
                // Active operations also contain phase/pending placeholders. An
                // unrecognized state must not become an apparently resolved
                // owner just because the history decoder ignores that state.
                if item["state"] != "pending"
                    && ItemOutcome::from_result(item).is_none()
                    && OperationPhase::deserialize(&item["state"]).is_err()
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
impl Store {
    fn new(directory: String) -> Self {
        Self {
            directory,
            frames: BTreeMap::new(),
            acknowledged: Vec::new(),
            scratch: Vec::new(),
            legacy_pending: false,
            error: None,
            load_error: false,
        }
    }
    pub(super) fn blocked(&self) -> bool {
        self.error.is_some()
    }
    pub(super) fn view(&self) -> Value {
        json!({"schema":2,"state":if self.blocked() {"blocked"} else {"durable"},"error":self.error,"devices":self.frames.len(),"files":1,"per_device_limit":MAX_BYTES,"total_limit":MAX_TOTAL_BYTES,"durable_bytes":self.acknowledged.len()})
    }
    fn file_path(&self) -> String {
        if self.directory == DIRECTORY {
            return crate::provisioning::runtime_store::Record::Recovery.path("");
        }
        format!("{}.json", self.directory)
    }
    fn index_path(&self) -> String {
        format!("{}/index.json", self.directory)
    }
    fn path(&self, id: &str) -> String {
        storage::registry_path(&self.directory, id)
    }
    fn load(&mut self) -> io::Result<bool> {
        match read_bounded(&self.file_path(), MAX_TOTAL_BYTES) {
            Ok(bytes) => {
                let file: RecoveryFile = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if file.schema != 2 || file.devices.len() > MAX_DEVICES {
                    return Err(invalid());
                }
                for (id, frame) in &file.devices {
                    if id != &frame.device {
                        return Err(invalid());
                    }
                    check_frame(frame)?;
                }
                self.frames = file.devices;
                self.legacy_pending = std::path::Path::new(&self.directory).is_dir();
                // Re-establish all barriers after process restart, including
                // identical bytes left by an unacknowledged rename.
                self.acknowledged.clear();
                return Ok(true);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e), // Never fall back to stale split files.
        }
        let loaded = self.load_split()?;
        self.legacy_pending = loaded;
        Ok(loaded)
    }
    fn load_split(&mut self) -> io::Result<bool> {
        let bytes = match read_bounded(&self.index_path(), MAX_BYTES) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::read_dir(&self.directory) {
                    Ok(entries) => {
                        for entry in entries {
                            if entry?.path().extension().is_some_and(|s| s == "json") {
                                return Err(invalid());
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        let index: Index = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if index.schema != 1 || index.devices.len() > MAX_DEVICES {
            return Err(invalid());
        }
        // Include newly renamed files from an interrupted index update. Missing
        // indexed files are corruption, never evidence that an owner disappeared.
        let mut files = BTreeSet::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "index.json" || name.ends_with(".tmp") {
                continue;
            }
            if !entry.file_type()?.is_file() || !name.ends_with(".json") {
                return Err(invalid());
            }
            files.insert(entry.path().to_string_lossy().into_owned());
            if files.len() > MAX_DEVICES {
                return Err(invalid());
            }
        }
        for id in &index.devices {
            if !files.contains(&self.path(id)) {
                return Err(invalid());
            }
        }
        let mut frames = BTreeMap::new();
        for file in files {
            let bytes = read_bounded(&file, MAX_BYTES)?;
            let frame: Snapshot = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            frame.validate()?;
            if file != self.path(&frame.device)
                || frames.insert(frame.device.clone(), frame).is_some()
            {
                return Err(invalid());
            }
        }
        self.frames = frames;
        // Re-establish file/directory barriers even when the previous process
        // saw the rename but never acknowledged its durability.
        self.acknowledged.clear();
        Ok(true)
    }
    fn save(&mut self, mut write: impl FnMut(&str, &[u8]) -> io::Result<()>) -> io::Result<()> {
        let result = self.save_inner(&mut write);
        if result.is_err() {
            self.acknowledged.clear();
        }
        result
    }
    fn save_inner(
        &mut self,
        mut write: impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.load_error {
            return Err(io::Error::other(
                "recovery load failed; operator repair required",
            ));
        }
        if self.frames.len() > MAX_DEVICES {
            return Err(invalid());
        }
        for frame in self.frames.values() {
            check_frame(frame)?;
        }
        let file = RecoveryView {
            schema: 2,
            devices: &self.frames,
        };
        let mut compare = Compare {
            acknowledged: &self.acknowledged,
            length: 0,
            equal: true,
            limit: MAX_TOTAL_BYTES,
        };
        serde_json::to_writer(&mut compare, &file)?;
        if !compare.equal || compare.length != self.acknowledged.len() {
            self.scratch.clear();
            // The bounded comparison above checks size before this allocation.
            serde_json::to_writer(&mut self.scratch, &file)?;
            write(&self.file_path(), &self.scratch)?;
            std::mem::swap(&mut self.acknowledged, &mut self.scratch);
        }
        if self.legacy_pending {
            // The single file is durable before retiring any predecessor. A
            // crash during cleanup always loads the new authority, not a subset.
            for id in self.frames.keys() {
                storage::remove_registry(&self.path(id)).map_err(io::Error::other)?;
            }
            storage::remove_registry(&self.index_path()).map_err(io::Error::other)?;
            self.legacy_pending = false;
        }
        Ok(())
    }
}

impl Controller {
    fn restore_frames(&mut self, store: &Store) {
        let operations: Vec<_> = store
            .frames
            .values()
            .filter_map(|f| f.operation.clone())
            .collect();
        let owned: BTreeMap<_, _> = store
            .frames
            .values()
            .filter(|f| !f.owned.is_empty())
            .map(|f| (f.cwmp_id.clone(), f.owned.clone()))
            .collect();
        self.recover(
            json!({"operations": operations, "owned": owned})
                .to_string()
                .as_bytes(),
        );
        for frame in store.frames.values() {
            self.attempts.insert(frame.device.clone(), frame.attempts);
            self.next_retry_at
                .insert(frame.device.clone(), frame.retry_at);
            self.automatic_ready
                .insert(frame.device.clone(), frame.reasons);
            self.read_recovery
                .insert(frame.device.clone(), frame.read.clone());
        }
    }
    pub(super) fn load_snapshots(&mut self) {
        let mut store = Store::new(DIRECTORY.into());
        match store.load() {
            Ok(true) => {
                self.restore_frames(&store);
            }
            Ok(false) => {
                let result = match read_bounded("/tmp/fttr/provisioning-state.json", LEGACY_BYTES) {
                    Ok(bytes) => {
                        // Validate before using the legacy decoder, which historically
                        // ignored malformed input. Do not silently migrate empty state.
                        serde_json::from_slice::<Value>(&bytes)
                            .map_err(|_| invalid())
                            .and_then(|v| {
                                let ops = v["operations"].as_array().ok_or_else(invalid)?;
                                if ops.len() > MAX_DEVICES || !v["owned"].is_object() {
                                    return Err(invalid());
                                }
                                for op in ops {
                                    if !op["device"].is_string()
                                        || !op["optical_sn"].is_string()
                                        || !op["items"].is_array()
                                    {
                                        return Err(invalid());
                                    }
                                }
                                self.recover(&bytes);
                                Ok(())
                            })
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                };
                if let Err(e) = result {
                    store.error = Some(e.to_string());
                    store.load_error = true;
                }
            }
            Err(e) => {
                store.error = Some(e.to_string());
                store.load_error = true;
            }
        }
        self.snapshots = Some(store);
        if self.persist_snapshots() {
            // Migration is durable before program_start.
            #[cfg(not(test))]
            if let Err(e) = storage::remove_registry("/tmp/fttr/provisioning-state.json") {
                eprintln!("[provisioning] retire obsolete UI cache: {e}");
            }
        }
    }

    pub(super) fn persist_snapshots(&mut self) -> bool {
        let Some(mut store) = self.snapshots.take() else {
            return true;
        };
        // Iterate the union without allocating a temporary identity tree on
        // every barrier. Preserve the original live > recovered > config
        // precedence, including owners removed from the current configuration.
        let configured = |id: &str| self.config.devices.iter().any(|d| d.id == id);
        let ids = self
            .config
            .devices
            .iter()
            .map(|d| d.id.as_str())
            .chain(
                self.recovered
                    .keys()
                    .filter(|id| !configured(id))
                    .map(String::as_str),
            )
            .chain(
                self.operations
                    .keys()
                    .filter(|id| !configured(id) && !self.recovered.contains_key(*id))
                    .map(String::as_str),
            );
        for id in ids {
            let identity = self
                .business_operation(id)
                .map(|op| (op.device.optical_sn.as_str(), op.device.cwmp_id.as_str()))
                .or_else(|| {
                    self.recovered
                        .get(id)
                        .and_then(|v| Some((v["optical_sn"].as_str()?, v["cwmp_id"].as_str()?)))
                })
                .or_else(|| {
                    self.config
                        .devices
                        .iter()
                        .find(|d| d.id == id)
                        .map(|d| (d.optical_sn.as_str(), d.cwmp_id.as_str()))
                });
            let Some((serial, cwmp_id)) = identity else {
                continue;
            };
            let retain = self.business_operation(id).is_some_and(|op| {
                op.facts().unresolved || (op.apply && op.stage != OperationPhase::Completed)
            }) || self
                .recovered
                .get(id)
                .is_some_and(|op| facts::Facts::recovered(op).unresolved);
            if !retain {
                store.frames.remove(id);
                continue;
            }
            let active = self.business_operation(id).map(Operation::view);
            let minimal = active
                .as_ref()
                .or_else(|| self.recovered.get(id))
                .map(intent_view);
            let operation = minimal.as_ref();
            if !store.frames.contains_key(id) {
                store.frames.insert(
                    id.into(),
                    Snapshot {
                        schema: 1,
                        serial: serial.into(),
                        cwmp_id: cwmp_id.into(),
                        operation: None,
                        owned: BTreeMap::new(),
                        attempts: 0,
                        retry_at: 0,
                        reasons: scheduler::Reasons::default(),
                        read: ReadRecovery::default(),
                        device: id.into(),
                    },
                );
            }
            let frame = store.frames.get_mut(id).unwrap();
            if frame.serial != serial {
                frame.serial = serial.into();
            }
            if frame.cwmp_id != cwmp_id {
                frame.cwmp_id = cwmp_id.into();
            }
            // Completed histories dominate idle ticks. Compare borrowed state
            // before cloning it, instead of rebuilding every JSON tree/map.
            if frame.operation.as_ref() != operation {
                // Active views are already owned temporaries. Move them into
                // the checkpoint instead of cloning a second full JSON tree.
                frame.operation = minimal;
            }
            // Only unresolved intent is a restart checkpoint. Derived histories,
            // duplicate ownership, timers and retry episodes stay in memory.
            frame.owned.clear();
            frame.attempts = 0;
            frame.retry_at = 0;
            frame.reasons = scheduler::Reasons::default();
            frame.read = ReadRecovery::default();
        }
        let success = match store.save(|path, bytes| storage::write_atomic(path, bytes, true)) {
            Ok(()) => {
                store.error = None;
                true
            }
            Err(e) => {
                store.acknowledged.clear();
                store.error = Some(format!("durable recovery snapshot: {e}"));
                false
            }
        };
        if !success {
            self.failure = store.error.clone();
        }
        self.snapshots = Some(store);
        success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::test_io::{self, Point};

    #[test]
    fn completed_history_and_retry_timers_are_not_restart_records() {
        let mut c = Controller::default();
        let op = super::super::tests::operation(OperationPhase::Completed, false);
        let id = op.device.id.clone();
        c.config.devices.push(op.device.clone());
        c.operations.insert(id.clone(), op);
        c.attempts.insert(id.clone(), 5);
        c.read_recovery.insert(
            id.clone(),
            ReadRecovery {
                attempts: 3,
                retry_at: 999,
            },
        );
        c.snapshots = Some(store("minimal"));
        assert!(c.persist_snapshots());
        assert!(c.snapshots.as_ref().unwrap().frames.is_empty());
        let mut op = super::super::tests::operation(OperationPhase::Writing, true);
        op.results.insert(
            "example".into(),
            json!({"state":"unknown", "actual":"discard-me"}),
        );
        c.operations.insert(id.clone(), op);
        assert!(c.persist_snapshots());
        let frame = &c.snapshots.as_ref().unwrap().frames[&id];
        assert_eq!(frame.attempts, 0);
        assert_eq!(frame.read.attempts, 0);
        assert!(frame.owned.is_empty());
        assert!(!serde_json::to_string(&frame.operation)
            .unwrap()
            .contains("discard-me"));
        assert!(facts::Facts::recovered(frame.operation.as_ref().unwrap()).unresolved);
    }

    fn store(name: &str) -> Store {
        Store::new(test_io::registry_directory(&format!("{DIRECTORY}/{name}")))
    }
    fn frame() -> Snapshot {
        let mut op = super::super::tests::operation(OperationPhase::Writing, true);
        op.id = 77;
        Snapshot {
            schema: 1,
            device: op.device.id.clone(),
            serial: op.device.optical_sn.clone(),
            cwmp_id: op.device.cwmp_id.clone(),
            operation: Some(op.view()),
            owned: BTreeMap::new(),
            attempts: 5,
            retry_at: 900,
            reasons: scheduler::Reasons(3),
            read: ReadRecovery {
                attempts: 2,
                retry_at: 800,
            },
        }
    }
    fn write(path: &str, bytes: &[u8]) -> io::Result<()> {
        storage::write_atomic(path, bytes, true)
    }

    #[test]
    fn restart_retains_exhausted_budget_original_owner_and_interrupted_intent() {
        let mut s = store("restart");
        let f = frame();
        s.frames.insert(f.device.clone(), f.clone());
        s.save(write).unwrap();
        let mut loaded = Store::new(s.directory.clone());
        assert!(loaded.load().unwrap());
        assert!(
            loaded.acknowledged.is_empty(),
            "restart re-establishes durability barriers"
        );
        let mut c = Controller::default();
        c.restore_frames(&loaded); // No configured device: orphan still survives.
        assert_eq!(c.attempts[&f.device], 5);
        assert_eq!(c.next_retry_at[&f.device], 900);
        assert_eq!(c.read_recovery[&f.device].attempts, 2);
        assert_eq!(c.read_recovery[&f.device].retry_at, 800);
        assert!(c.previous_facts(&f.device).unwrap().unresolved);
        assert_eq!(c.recovered[&f.device]["operation_id"], 77);
        assert!(c.next_id >= 77);
    }

    #[test]
    fn failed_barrier_cannot_be_acknowledged_by_identical_content_or_reversion() {
        for point in [Point::FileSync, Point::Rename, Point::DirectorySync] {
            let mut s = store(&format!("barrier-{point:?}"));
            let f = frame();
            s.frames.insert(f.device.clone(), f.clone());
            test_io::fail_next(point);
            assert!(s.save(write).is_err());
            assert!(s.acknowledged.is_empty());
            test_io::fail_next(point);
            assert!(s.save(write).is_err());
            s.save(write).unwrap();
            s.frames.get_mut(&f.device).unwrap().attempts = 4;
            test_io::fail_next(Point::DirectorySync);
            assert!(s.save(write).is_err());
            s.frames.get_mut(&f.device).unwrap().attempts = 5;
            let mut writes = 0;
            s.save(|path, bytes| {
                writes += 1;
                write(path, bytes)
            })
            .unwrap();
            assert!(
                writes > 0,
                "old ack was invalidated after failed changed snapshot"
            );
        }
    }

    #[test]
    fn missing_schema_identity_budget_and_size_evidence_fail_closed() {
        for mode in 0..9 {
            let mut s = store(&format!("invalid-{mode}"));
            let f = frame();
            s.frames.insert(f.device.clone(), f.clone());
            s.save(write).unwrap();
            let path = s.file_path();
            let encode = |f: Snapshot| {
                serde_json::to_vec(&RecoveryView {
                    schema: 2,
                    devices: &BTreeMap::from([(f.device.clone(), f)]),
                })
                .unwrap()
            };
            match mode {
                0 => {
                    storage::remove_registry(&path).unwrap();
                    write(
                        &s.index_path(),
                        &serde_json::to_vec(&Index {
                            schema: 1,
                            devices: BTreeSet::from([f.device.clone()]),
                        })
                        .unwrap(),
                    )
                    .unwrap();
                }
                1 => {
                    let mut f = f.clone();
                    f.schema = 2;
                    write(&path, &encode(f)).unwrap();
                }
                2 => {
                    let mut f = f.clone();
                    f.attempts = 6;
                    write(&path, &encode(f)).unwrap();
                }
                3 => {
                    let mut f = f.clone();
                    f.operation.as_mut().unwrap()["optical_sn"] = json!("TEST_OTHER");
                    write(&path, &encode(f)).unwrap();
                }
                4 => write(&path, &vec![b' '; MAX_TOTAL_BYTES + 1]).unwrap(),
                5 => write(&path, b"{corrupt").unwrap(),
                6 => write(&path, b"{\"schema\":3,\"devices\":{}}").unwrap(),
                7 | 8 => {
                    let mut f = f.clone();
                    if mode == 7 {
                        f.operation.as_mut().unwrap()["state"] = json!("unrecognized_phase");
                    } else {
                        f.operation.as_mut().unwrap()["items"][0]["state"] =
                            json!("unrecognized_outcome");
                    }
                    write(&path, &encode(f)).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                Store::new(s.directory.clone()).load().is_err(),
                "mode {mode}"
            );
        }
    }

    #[test]
    fn unindexed_rename_and_removed_configuration_cannot_drop_an_orphan() {
        let mut s = store("orphan");
        write(&s.index_path(), br#"{"schema":1,"devices":[]}"#).unwrap();
        let f = frame();
        write(&s.path(&f.device), &serde_json::to_vec(&f).unwrap()).unwrap();
        assert!(s.load().unwrap());
        assert_eq!(s.frames.len(), 1);
        let mut c = Controller::default();
        c.restore_frames(&s);
        c.snapshots = Some(s);
        assert!(c.persist_snapshots());
        assert_eq!(c.snapshots.as_ref().unwrap().frames.len(), 1);
    }

    #[test]
    fn corrupt_load_and_failed_save_never_admit_a_new_operation() {
        let mut c = Controller::default();
        let op = super::super::tests::operation(OperationPhase::Completed, false);
        let id = op.device.id.clone();
        c.config.devices.push(op.device);
        let mut s = store("blocked");
        s.load_error = true;
        s.error = Some("invalid snapshot".into());
        c.snapshots = Some(s);
        assert!(!c.persist_snapshots());
        assert!(c
            .start(&id, false, false)
            .unwrap_err()
            .contains("durable recovery state"));
        assert!(!c.persist_snapshots());
        assert!(!std::path::Path::new(&c.snapshots.as_ref().unwrap().file_path()).exists());
    }

    #[test]
    fn split_migration_retires_files_only_after_single_file_commit_and_never_falls_back() {
        let mut s = store("migration");
        let f = frame();
        write(
            &s.index_path(),
            &serde_json::to_vec(&Index {
                schema: 1,
                devices: BTreeSet::from([f.device.clone()]),
            })
            .unwrap(),
        )
        .unwrap();
        write(&s.path(&f.device), &serde_json::to_vec(&f).unwrap()).unwrap();
        assert!(s.load().unwrap());
        test_io::fail_next(Point::FileSync);
        assert!(s.save(write).is_err());
        assert!(std::path::Path::new(&s.path(&f.device)).exists());
        assert!(std::path::Path::new(&s.index_path()).exists());
        s.save(write).unwrap();
        assert!(!std::path::Path::new(&s.path(&f.device)).exists());
        assert!(!std::path::Path::new(&s.index_path()).exists());
        assert!(Store::new(s.directory.clone()).load().unwrap());
        write(&s.file_path(), b"corrupt new authority").unwrap();
        // Even a valid stale predecessor must never override a corrupt authority.
        write(&s.index_path(), br#"{"schema":1,"devices":[]}"#).unwrap();
        assert!(Store::new(s.directory).load().is_err());
    }

    #[test]
    fn stable_checkpoints_reuse_trees_and_buffers_without_io_or_duplicate_history() {
        let mut c = Controller::default();
        let mut s = store("stable");
        let f = frame();
        s.frames.insert(f.device.clone(), f.clone());
        c.restore_frames(&s);
        c.snapshots = Some(s);
        assert!(c.persist_snapshots());
        let before = c.snapshots.as_ref().unwrap();
        let tree = before.frames[&f.device].operation.as_ref().unwrap() as *const Value;
        let bytes = before.acknowledged.as_ptr();
        test_io::events();
        for _ in 0..20 {
            assert!(c.persist_snapshots());
        }
        let after = c.snapshots.as_ref().unwrap();
        assert_eq!(
            tree,
            after.frames[&f.device].operation.as_ref().unwrap() as *const Value
        );
        assert_eq!(bytes, after.acknowledged.as_ptr());
        assert!(
            test_io::events().is_empty(),
            "unchanged frames must not hit persistence barriers"
        );
        c.persist_history(|_| panic!("no duplicate legacy UI file"));
        assert!(c.saved.is_empty());
    }

    #[test]
    fn duplicate_devices_and_individual_or_aggregate_oversize_fail_closed() {
        let mut s = store("bounds");
        let f = frame();
        let device = serde_json::to_string(&f.device).unwrap();
        let bytes = serde_json::to_string(&f).unwrap();
        write(
            &s.file_path(),
            format!("{{\"schema\":2,\"devices\":{{{device}:{bytes},{device}:{bytes}}}}}")
                .as_bytes(),
        )
        .unwrap();
        assert!(s.load().is_err());
        let mut huge = f.clone();
        huge.operation.as_mut().unwrap()["padding"] = json!("x".repeat(MAX_BYTES));
        s.frames.insert(huge.device.clone(), huge);
        assert!(s.save(|_, _| panic!("oversize cannot write")).is_err());
        s.frames.clear();
        for n in 0..200 {
            let mut f = f.clone();
            f.device = format!("device-{n}");
            let op = f.operation.as_mut().unwrap();
            op["device"] = json!(f.device);
            op["padding"] = json!("x".repeat(24 * 1024));
            s.frames.insert(f.device.clone(), f);
        }
        assert!(s
            .save(|_, _| panic!("aggregate bound cannot write"))
            .is_err());
    }
}
