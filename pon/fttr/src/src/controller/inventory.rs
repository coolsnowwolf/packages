//! Read-only identity/MIB operations coordinated with the registered ONU worker.
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::{mib::MibReport, OmciSession};
mod tables;
use std::sync::atomic::AtomicU64;
use std::{
    fs::{self, OpenOptions},
    io,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

static NEXT_PROGRESS_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(serde::Serialize)]
pub(crate) struct MibProgress {
    pub phase: &'static str,
    pub received: usize,
    pub expected: Option<usize>,
}

#[derive(Clone)]
pub(crate) enum MibProgressReporter {
    File(PathBuf),
    Job(fttr_jobs::Progress),
}

impl MibProgressReporter {
    pub fn new(id: &str) -> io::Result<Self> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid MIB progress identifier",
            ));
        }
        Ok(Self::File(PathBuf::from(format!(
            "/tmp/fttr-mib.{id}/progress"
        ))))
    }

    pub fn update(&self, progress: &MibProgress) -> io::Result<()> {
        let path = match self {
            Self::File(path) => path,
            Self::Job(reporter) => {
                let phase = match progress.phase {
                    "starting" => fttr_jobs::Phase::Starting,
                    "upload" => fttr_jobs::Phase::Upload,
                    "tables" => fttr_jobs::Phase::Tables,
                    _ => return Err(io::Error::other("Invalid MIB progress phase")),
                };
                reporter.update(phase, progress.received, progress.expected);
                return Ok(());
            }
        };
        let directory = path.parent().expect("MIB progress path has a parent");
        if !fs::symlink_metadata(directory)?.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid MIB job directory",
            ));
        }
        let sequence = NEXT_PROGRESS_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!("progress.{}.{}.tmp", std::process::id(), sequence));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, progress).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        fs::rename(temporary, path)
    }
}

pub(super) fn collect(
    platform: PlatformContext,
    id: u8,
    cancellation: Arc<AtomicBool>,
) -> io::Result<crate::model::OnuIdentity> {
    let _lock = lock_onu(id)?;
    if cancellation.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "identity collection cancelled",
        ));
    }
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || super::service::operation_confirmed(id));
    session.cancel_when(cancellation.clone());
    let _lease = platform.management_lease(id)?;
    let _channel = platform.channel(id, 0)?;
    let mut identity = crate::model::OnuIdentity::default();
    for (class, entity, mask, label) in [
        (257, 0, 0x8000, "equipment"),
        (256, 0, 0x4000, "hardware"),
        (257, 0, 0x4000, "OMCC"),
        (7, 0, 0x8000, "software image 0"),
        (7, 1, 0x8000, "software image 1"),
    ] {
        match session.get_attributes(class, entity, mask) {
            Ok(block) => {
                let value = &block
                    .attributes
                    .first()
                    .ok_or_else(|| io::Error::other("empty identity attribute"))?
                    .value;
                match label {
                    "equipment" => {
                        identity.equipment_id = value
                            .text()
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.trim().to_string())
                    }
                    "hardware" => {
                        identity.hardware_version = value
                            .text()
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.trim().to_string())
                    }
                    "OMCC" => identity.omcc_version = value.unsigned().map(|v| v as u8),
                    _ => {
                        if let Some(version) = value.text().filter(|s| !s.trim().is_empty()) {
                            identity
                                .software_images
                                .push(format!("image {entity}: {}", version.trim()));
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotConnected | io::ErrorKind::Interrupted
                ) =>
            {
                return Err(error)
            }
            Err(error) => identity.errors.push(format!("{label}: {error}")),
        }
    }
    Ok(identity)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MibCommand {
    pub onu_id: u8,
    pub tables: bool,
    #[serde(default)]
    pub progress_id: Option<String>,
}
pub(crate) struct MibRequest {
    pub tables: bool,
    pub deadline: Instant,
    pub progress: Option<MibProgressReporter>,
    pub reply: std::sync::mpsc::Sender<io::Result<MibReport>>,
    pub registration: Option<RegistrationGuard>,
}
pub(crate) type RegistrationGuard = Arc<dyn Fn() -> bool + Send + Sync>;
pub(crate) enum WorkerRequest {
    Mib(MibRequest),
    Attribute(super::attribute_probe::Request),
    Fec(super::fec::Request),
}

/// Default output is one JSON document; human-readable summary is a client view.
pub fn read(id: u8, summary: bool, tables: bool) -> io::Result<()> {
    read_with_progress(id, summary, tables, None)
}

pub fn read_with_progress(
    id: u8,
    summary: bool,
    tables: bool,
    progress_id: Option<String>,
) -> io::Result<()> {
    if !crate::pon_engine::onu_id::is_valid(u16::from(id)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "ONU_ID must be {}..{}; availability depends on the selected platform",
                crate::pon_engine::onu_id::MIN_ONU_ID,
                crate::pon_engine::onu_id::MAX_ONU_ID
            ),
        ));
    }
    let command = serde_json::to_string(&MibCommand {
        onu_id: id,
        tables,
        progress_id,
    })
    .map_err(io::Error::other)?;
    let response = crate::ipc::send_ipc_command(&format!("MIB {command}"))?;
    let value: serde_json::Value = serde_json::from_str(&response).map_err(io::Error::other)?;
    if let Some(error) = value.get("error") {
        return Err(io::Error::other(
            error["message"]
                .as_str()
                .unwrap_or("MIB operation failed")
                .to_string(),
        ));
    }
    if value["schema_version"] != 1 {
        return Err(io::Error::other("invalid MIB report schema"));
    }
    if !summary {
        println!("{response}");
        return Ok(());
    }
    println!(
        "MIB ONU={} upload_complete={} records={} decoded={} partial={} basic={}",
        id,
        value["uploaded_complete"],
        value["received_records"],
        value["decoded_records"],
        value["partial_records"],
        value["basic_records"]
    );
    if let Some(classes) = value["classes"].as_array() {
        for class in classes {
            println!(
                "CLASS {} {} entities={} records={} support={}",
                class["class_id"],
                class["name"].as_str().unwrap_or("vendor/unknown"),
                class["entities"],
                class["records"],
                class["support"]
            );
        }
    }
    if let Some(entities) = value["entities"].as_array() {
        for entity in entities.iter().filter(|e| {
            [7, 11, 171, 256, 257, 262, 266, 268, 329]
                .contains(&(e["class_id"].as_u64().unwrap_or(0) as u16))
        }) {
            println!(
                "ENTITY {}:{:04x} {}",
                entity["class_id"],
                entity["entity_id"].as_u64().unwrap_or(0),
                entity["name"].as_str().unwrap_or("unknown")
            );
            if let Some(attrs) = entity["attributes"].as_array() {
                for attr in attrs {
                    println!(
                        "  {} = {}",
                        attr["definition"]["name"].as_str().unwrap_or("unknown"),
                        attr["value"]
                    );
                }
            }
        }
    }
    println!(
        "TABLES requested={} collected={}; read-only",
        value["tables_requested"], value["tables_collected"]
    );
    Ok(())
}

/// Caller owns the ONU lock and OMCC/channel leases; session owns only transactions.
pub(crate) fn read_owned(
    platform: PlatformContext,
    id: u8,
    tables: bool,
    cancellation: Option<Arc<AtomicBool>>,
    deadline: Instant,
    progress: Option<MibProgressReporter>,
    registration: Option<RegistrationGuard>,
) -> io::Result<MibReport> {
    if !super::service::operation_confirmed(id)
        || registration.as_ref().is_some_and(|guard| !guard())
    {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "MIB requires a currently registered ONU",
        ));
    }
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || {
        super::service::operation_confirmed(id) && registration.as_ref().is_none_or(|guard| guard())
    });
    session.finish_before(deadline);
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    if let Some(reporter) = &progress {
        reporter.update(&MibProgress {
            phase: "upload",
            received: 0,
            expected: None,
        })?;
    }
    let mut report = session.upload_mib_with_progress(|received, expected| {
        if let Some(reporter) = &progress {
            reporter.update(&MibProgress {
                phase: "upload",
                received,
                expected: Some(expected),
            })?;
        }
        Ok(())
    })?;
    report.onu_id = Some(id);
    report.tables_requested = tables;
    if tables {
        let expected = report
            .entities
            .iter()
            .filter(|entity| entity.class_id == 171)
            .count();
        if let Some(reporter) = &progress {
            reporter.update(&MibProgress {
                phase: "tables",
                received: 0,
                expected: Some(expected),
            })?;
        }
        self::tables::collect(&mut session, &mut report, |received, expected| {
            if let Some(reporter) = &progress {
                reporter.update(&MibProgress {
                    phase: "tables",
                    received,
                    expected: Some(expected),
                })?;
            }
            Ok(())
        })?;
    }
    Ok(report)
}
pub(crate) fn control(
    platform: PlatformContext,
    id: u8,
    tables: bool,
    progress: Option<MibProgressReporter>,
    states: &super::service::States,
) -> io::Result<MibReport> {
    control_guarded(platform, id, tables, progress, states, None)
}

pub(crate) fn control_guarded(
    platform: PlatformContext,
    id: u8,
    tables: bool,
    progress: Option<MibProgressReporter>,
    states: &super::service::States,
    registration: Option<RegistrationGuard>,
) -> io::Result<MibReport> {
    platform.resources().onu(id)?;
    if !super::service::operation_confirmed(id) {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "MIB requires a currently registered ONU",
        ));
    }
    // Some baseline ONUs pace Upload-NEXT responses at roughly one per second.
    // A complete, ~200-record MIB must still fit within the bounded diagnostic.
    let deadline = Instant::now() + Duration::from_secs(600);
    match lock_onu(id) {
        Ok(_lock) => {
            if !super::service::operation_confirmed(id) {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "ONU no longer registered",
                ));
            }
            let _lease = platform.management_lease(id)?;
            let _channel = platform.channel(id, 0)?;
            read_owned(platform, id, tables, None, deadline, progress, registration)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let (sender, wake) = states
                .lock()
                .unwrap()
                .get(&id)
                .and_then(|s| Some((s.mib.clone()?, s.wake.clone()?)))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::WouldBlock, "ONU busy; no active monitor")
                })?;
            let (reply, receive) = std::sync::mpsc::channel();
            sender
                .try_send(WorkerRequest::Mib(MibRequest {
                    tables,
                    deadline,
                    progress,
                    reply,
                    registration,
                }))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "MIB queue busy or stopped")
                })?;
            wake.notify();
            receive
                .recv_timeout(Duration::from_secs(605))
                .map_err(|e| io::Error::other(format!("MIB worker unavailable: {e}")))?
        }
        Err(error) => Err(error),
    }
}
