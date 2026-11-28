//! AN4475-G2 FSL hardware owner, runtime scheduler, and OMCI/PLOAM bridge.
use super::{
    activation::{
        assign_alloc_id_ploam, configure_gem_port, set_gem_wmci_aes, PLOAM_MSG_ACK,
        PLOAM_MSG_ASSIGN_ALLOC_ID, PLOAM_MSG_BER_INTERVAL,
    },
    gdba::{alloc_to_tcont, GdbaAllocConfig},
    olt::{FslOltSession, OltSessionConfig, OnuState as FslOnuState, OnuUpstreamRate},
    protocol::Bus,
    transport::Device,
};
use crate::{
    controller::{
        ipc_server, lifetime,
        optical_owner::{OpticalActor, OpticalBackend, OpticalClient, OpticalCommand},
        service,
        ubus_mib::UBUS_ERROR_INTERNAL,
    },
    ipc::{self, SubDevice},
    platform::{
        context::{ChannelGuard, ManagementLease, PlatformContext},
        openwrt::network,
    },
    pon_engine::{
        omci::{session::OmciTransport, wire::BaselineFrame},
        ploam::PloamFrame,
        PonEngine,
    },
};
use fttr_api::OpticalState;
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{self, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

const FSL_DEVICE_PATH: &str = "/dev/fsl-1fbd4000.fsl";
const NNI_INTERFACE: &str = "fttr0";
const SERIAL_NUMBER_ONU_MSG_ID: u8 = 1;
const PLOAM_MSG_DYING_GASP: u8 = 3;

fn deactivate_session_onu(session: &mut FslOltSession<Device>, id: u8) -> io::Result<bool> {
    if session.onu(id).is_none() {
        return Ok(false);
    }
    let res = super::active_resources().onu(id)?;
    let max_onus = session.config().init.max_onu_id.min(128) as u8;
    let extra_tconts = alloc_to_tcont(res.alloc_id, 0, max_onus, 0)
        .map(|t| vec![t])
        .unwrap_or_default();
    session.deactivate_onu(id, &extra_tconts, &[res.gem_id], &mut |delay| {
        thread::sleep(delay);
        Ok(())
    })?;
    Ok(true)
}

/// Bounded owner-only readback, including the counter banks read by stock
/// `fsldrv_gpon_getponstat`. Do not acknowledge status, clear counters, or read
/// the PLOAM/OMCI FIFO here. Counter values are raw snapshots, not interval sums.
fn log_receive_path_snapshot(session: &mut FslOltSession<Device>) -> io::Result<()> {
    use crate::platform::diagnostics::RegisterSpace;
    for (name, address, words) in [
        ("pon_reset", 0x12, 1),
        ("ram_status", 0x31, 1),
        ("serdes_status", 0x110f, 1),
        ("opt_config", 0x102, 1),
        ("sd_config", 0x208, 1),
        ("gpon_mask", 0x210, 1),
        ("gpon_int", 0x2f0, 1),
        ("ploam_mask", 0x44a, 1),
        ("ploam_int", 0x49e, 1),
        ("ploam_valid", 0x41b, 1),
        ("sn_request", 0x606, 2),
        ("dba_start", 0x501, 1),
        ("dba_work", 0x500, 1),
        ("nni_spi", 0x913, 1),
        ("opt_int", 0x150, 1),
        ("us_gtc_counters", 0x732, 14),
        ("ds_gem_counters", 0x74c, 14),
    ] {
        let values = session
            .fsl_mut()
            .read(RegisterSpace::Register, address, 0, words)?;
        println!("[*] FSL RX snapshot {name}@{address:#x}={values:x?}");
    }
    Ok(())
}

/// Shared runtime slots. Optical replies always report the actual TX register
/// via read_tx (FSL has no independently verified BOSA RESET_N control whose
/// state a software flag could be trusted over the hardware).
struct FslRuntimeState {
    session: Mutex<Option<FslOltSession<Device>>>,
    omci_rx: Mutex<BTreeMap<u8, VecDeque<BaselineFrame>>>,
    cleanup_pending: AtomicBool,
}

static RUNTIME: OnceLock<FslRuntimeState> = OnceLock::new();

fn runtime_state() -> &'static FslRuntimeState {
    RUNTIME.get_or_init(|| FslRuntimeState {
        session: Mutex::new(None),
        omci_rx: Mutex::new(BTreeMap::new()),
        cleanup_pending: AtomicBool::new(false),
    })
}

fn with_session<T>(f: impl FnOnce(&mut FslOltSession<Device>) -> io::Result<T>) -> io::Result<T> {
    let mut guard = runtime_state()
        .session
        .lock()
        .map_err(|_| io::Error::other("FSL runtime session lock poisoned"))?;
    let session = guard.as_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "FSL runtime session is not initialized",
        )
    })?;
    f(session)
}

pub(crate) struct FslOmciTransport {
    onu_id: u8,
    deadline: Option<Instant>,
    request: Option<BaselineFrame>,
}

impl FslOmciTransport {
    pub(crate) fn new(onu_id: u8) -> Self {
        Self {
            onu_id,
            deadline: None,
            request: None,
        }
    }
}

// A poll slice may expire after the irreversible submit write. Finish observing
// that single handshake under a separate small budget, without replaying it.
// Kernel I/O itself remains non-interruptible; the independent port gate still
// enforces liveness while the owner is blocked.
struct OmciSendCheckpoint {
    preparation: Option<Instant>,
    completion: Option<Instant>,
}

impl OmciSendCheckpoint {
    fn check(&mut self, phase: super::downstream::TxPhase, delay: Duration) -> io::Result<()> {
        let deadline = match phase {
            super::downstream::TxPhase::Preparing => self.preparation,
            super::downstream::TxPhase::Completing => Some(
                *self
                    .completion
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(50)),
            ),
        };
        let check = || {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("FSL OMCI {phase:?} deadline exceeded"),
                ))
            } else {
                Ok(())
            }
        };
        check()?;
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        check()
    }
}

fn lock_before<T>(
    mutex: &Mutex<T>,
    deadline: Option<Instant>,
) -> io::Result<std::sync::MutexGuard<'_, T>> {
    let Some(deadline) = deadline else {
        return mutex
            .lock()
            .map_err(|_| io::Error::other("FSL mutex poisoned"));
    };
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "FSL mutex deadline exceeded",
            ));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("FSL mutex poisoned"))
            }
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
        }
    }
}

impl OmciTransport for FslOmciTransport {
    fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }

    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        let bytes = frame.to_bytes();
        if self.request.as_ref().is_none_or(|previous| {
            previous.transaction_id != frame.transaction_id
                || previous.message_type != frame.message_type
        }) {
            // Discard old replies if the finite TCI space has wrapped. A paused
            // request is not sent again until its ordinary retry timeout.
            let mut queues = lock_before(&runtime_state().omci_rx, self.deadline)?;
            if let Some(queue) = queues.get_mut(&self.onu_id) {
                queue.retain(|reply| !reply.answers(frame));
            }
        }
        let mut guard = lock_before(&runtime_state().session, self.deadline)?;
        let session = guard.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "FSL runtime session is not initialized",
            )
        })?;
        let mut checkpoint = OmciSendCheckpoint {
            preparation: self.deadline,
            completion: None,
        };
        let result = session.send_omci_with_phases(self.onu_id, &bytes, &mut |phase, delay| {
            checkpoint.check(phase, delay)
        });
        if let Err(error) = &result {
            if session.needs_reconciliation() {
                eprintln!(
                    "[-] FSL OMCI send for ONU {} requires reconciliation: {error}",
                    self.onu_id
                );
            }
        }
        result?;
        self.request = Some(*frame);
        Ok(())
    }

    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        let mut queues = lock_before(&runtime_state().omci_rx, self.deadline)?;
        Ok(queues
            .get_mut(&self.onu_id)
            .and_then(|queue| take_omci_response(queue, self.request.as_ref())))
    }
}

// A paused upload can still have a reply in flight while a fresh liveness GET
// runs in another session. Only remove this transport's matching response.
fn take_omci_response(
    queue: &mut VecDeque<BaselineFrame>,
    request: Option<&BaselineFrame>,
) -> Option<BaselineFrame> {
    let request = request?;
    let index = queue.iter().position(|frame| frame.answers(request))?;
    queue.remove(index)
}

fn queue_omci_response(queue: &mut VecDeque<BaselineFrame>, frame: BaselineFrame) {
    // Unsolicited and abandoned replies must not grow the per-ONU queue forever.
    if queue.len() >= 512 {
        queue.pop_front();
    }
    queue.push_back(frame);
}

/// OMCI responses are queued per raw source (upstream metadata bits 21:14),
/// which only routes meaningfully while that value matches a live ONU. With the
/// offset-0 invariant enforced at initialize, source == ONU-ID holds; accepting
/// anything else would grow unbounded queues for unknown sources and mask a
/// wrong field interpretation behind silent drops.
pub(super) fn omci_source_is_live<B: Bus>(session: &FslOltSession<B>, source: u8) -> bool {
    session
        .onu(source)
        .is_some_and(|onu| onu.state == FslOnuState::Active)
}

#[cfg(test)]
mod omci_response_tests {
    use super::*;

    #[test]
    fn submitted_handshake_has_a_bounded_completion_budget_independent_of_poll_slice() {
        use super::super::downstream::TxPhase;
        let past = Instant::now() - Duration::from_millis(1);
        let mut checkpoint = OmciSendCheckpoint {
            preparation: Some(past),
            completion: None,
        };
        assert_eq!(
            checkpoint
                .check(TxPhase::Preparing, Duration::ZERO)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        checkpoint
            .check(TxPhase::Completing, Duration::ZERO)
            .unwrap();
        let completion = checkpoint.completion.unwrap();
        checkpoint
            .check(TxPhase::Completing, Duration::ZERO)
            .unwrap();
        assert_eq!(
            checkpoint.completion,
            Some(completion),
            "polls cannot extend the completion deadline"
        );
        checkpoint.completion = Some(past);
        assert_eq!(
            checkpoint
                .check(TxPhase::Completing, Duration::ZERO)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn liveness_get_preserves_late_upload_reply() {
        let upload = BaselineFrame::new(1, 0x4d, 2, 0, [0; 32]);
        let get = BaselineFrame::new(2, 0x49, 257, 0, [0; 32]);
        let upload_reply = BaselineFrame::new(1, 0x2d, 2, 0, [0; 32]);
        let get_reply = BaselineFrame::new(2, 0x29, 257, 0, [0; 32]);
        let mut queue = VecDeque::from([upload_reply, get_reply]);
        assert_eq!(
            take_omci_response(&mut queue, Some(&get))
                .unwrap()
                .transaction_id,
            2
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(
            take_omci_response(&mut queue, Some(&upload))
                .unwrap()
                .transaction_id,
            1
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn unmatched_frames_are_bounded_and_not_given_to_another_request() {
        let frame = BaselineFrame::new(3, 0x29, 257, 0, [0; 32]);
        let mut queue = VecDeque::new();
        for _ in 0..600 {
            queue_omci_response(&mut queue, frame);
        }
        assert_eq!(queue.len(), 512);
        let request = BaselineFrame::new(4, 0x49, 257, 0, [0; 32]);
        assert!(take_omci_response(&mut queue, Some(&request)).is_none());
        assert_eq!(queue.len(), 512);
    }
}

pub(crate) struct FslManagementLease {
    onu_id: u8,
    disarmed: bool,
    business_modified: bool,
}

impl FslManagementLease {
    pub(crate) fn open(onu_id: u8) -> io::Result<Self> {
        if usize::from(onu_id) >= super::active_resources().onu_ids.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONU-ID out of AN4475-G2 range",
            ));
        }
        Ok(Self {
            onu_id,
            disarmed: false,
            business_modified: false,
        })
    }
}

fn commit_business(
    modified: &mut bool,
    disarmed: &mut bool,
    configure: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if !*modified {
        // A failed configure can still have partially changed hardware.
        *modified = true;
        configure()?;
    }
    *disarmed = true;
    Ok(())
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    #[test]
    fn contended_runtime_lock_respects_deadline() {
        let lock = Mutex::new(());
        let _held = lock.lock().unwrap();
        let error =
            lock_before(&lock, Some(Instant::now() + Duration::from_millis(10))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn failed_business_commit_keeps_partial_configuration_armed_for_cleanup() {
        let (mut modified, mut disarmed) = (false, false);
        let error = commit_business(&mut modified, &mut disarmed, || {
            Err(io::Error::other("injected write failure"))
        });
        assert!(error.is_err());
        assert!(modified);
        assert!(!disarmed);
    }

    #[test]
    fn already_configured_business_commits_without_reconfiguration() {
        let (mut modified, mut disarmed) = (true, false);
        commit_business(&mut modified, &mut disarmed, || {
            panic!("unexpected reconfiguration")
        })
        .unwrap();
        assert!(disarmed);
    }
}

fn configure_business_datapath(onu_id: u8, floor_bytes: Option<u16>) -> io::Result<()> {
    let res = super::active_resources().onu(onu_id)?;
    with_session(|session| {
        let onu = session
            .onu(onu_id)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ONU not active in FSL"))?;
        let max_onus = session.config().init.max_onu_id.min(128) as u8;
        let tcont = alloc_to_tcont(res.alloc_id, 0, max_onus, 0)?;
        let alloc = GdbaAllocConfig::business_data(
            tcont,
            onu.onu_index,
            session.dba_granularity_8x(),
            onu.rate == OnuUpstreamRate::Rate2p5G,
            floor_bytes.map(u32::from),
        )?;
        // Match fslstack_gpon_config_alloc_id: send Assign_Alloc-ID PLOAM and allow
        // 5 ms before enabling GDBA grants; granting an unassigned Alloc-ID causes LOSi/LOFi.
        let frame = assign_alloc_id_ploam(onu_id, res.alloc_id, true, 0)?;
        for _ in 0..session.config().ploam_repeat {
            session.send_ploam(&frame, &mut |delay| {
                thread::sleep(delay);
                Ok(())
            })?;
        }
        thread::sleep(Duration::from_millis(5));
        session.configure_tcont_alloc(&alloc)?;
        configure_gem_port(session.fsl_mut(), onu.onu_index, 0, res.gem_id, true)?;
        println!(
            "[+] FSL business datapath enabled for ONU {onu_id}: gem={:#x} alloc={:#x} tcont={tcont} fixed_bw={} max_bw={} alloc_type={}",
            res.gem_id, res.alloc_id, alloc.fixed_bw, alloc.max_bw, alloc.alloc_type
        );
        Ok(())
    })
}

impl ManagementLease for FslManagementLease {
    fn set_business_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        // Mark before touching hardware so partial configuration is cleaned up.
        // A read-only management lease must not revoke persistent business.
        self.business_modified = true;
        if bytes == 0 {
            revoke_business(self.onu_id)
        } else {
            configure_business_datapath(self.onu_id, Some(bytes))
        }
    }

    fn set_management_grant_floor(&mut self, _bytes: u16) -> io::Result<()> {
        Ok(())
    }

    fn disarm(&mut self) -> io::Result<()> {
        commit_business(&mut self.business_modified, &mut self.disarmed, || {
            configure_business_datapath(self.onu_id, None)
        })
    }
}

impl Drop for FslManagementLease {
    fn drop(&mut self) {
        if self.business_modified && !self.disarmed {
            let _ = revoke_business(self.onu_id);
        }
    }
}

pub(crate) struct FslChannelGuard;
impl ChannelGuard for FslChannelGuard {}

pub(crate) fn send_ploam(frame: &PloamFrame) -> io::Result<()> {
    let bytes = super::olt::to_downstream_ploam(frame);
    with_session(|session| {
        session.send_ploam(&bytes, &mut |delay| {
            thread::sleep(delay);
            Ok(())
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum DatapathStage {
    Status,
    GemEnable,
    GemDisable,
    AssignAllocPloamEnable,
    AssignAllocPloamDisable,
    GdbaEnable,
    GdbaDisable,
    Revoke,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DatapathStageCommand {
    onu_id: u8,
    serial: String,
    generation: u64,
    stage: DatapathStage,
    #[serde(default)]
    fixed_bw: Option<u32>,
    #[serde(default)]
    max_bw: Option<u32>,
    #[serde(default)]
    ds_period: Option<u8>,
    #[serde(default)]
    ds_flag: Option<bool>,
    #[serde(default)]
    ds_tail_flag: Option<bool>,
    #[serde(default)]
    sequence: Option<u16>,
}

fn datapath_stage(
    cmd: DatapathStageCommand,
    states: &service::States,
) -> io::Result<serde_json::Value> {
    if !crate::controller::attribute_probe::identity_current(
        states,
        cmd.onu_id,
        &cmd.serial,
        cmd.generation,
    ) {
        return Err(io::Error::other(
            "ONU identity/registration generation mismatch",
        ));
    }
    let res = super::active_resources().onu(cmd.onu_id)?;
    with_session(|session| {
        let onu = session
            .onu(cmd.onu_id)
            .copied()
            .filter(|onu| onu.state == FslOnuState::Active)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ONU not active in FSL"))?;
        let max_onus = session.config().init.max_onu_id.min(128) as u8;
        let tcont = alloc_to_tcont(res.alloc_id, 0, max_onus, 0)?;
        let (losi_count, lofi_count) = session.alarm_counts(cmd.onu_id);
        match cmd.stage {
            DatapathStage::Status => Ok(serde_json::json!({
                "status": "success",
                "stage": "status",
                "onu_id": cmd.onu_id,
                "onu_index": onu.onu_index,
                "rate": format!("{:?}", onu.rate),
                "gem_id": res.gem_id,
                "alloc_id": res.alloc_id,
                "tcont": tcont,
                "dba_granularity_8x": session.dba_granularity_8x(),
                "losi_count": losi_count,
                "lofi_count": lofi_count,
            })),
            DatapathStage::GemEnable | DatapathStage::GemDisable => {
                let enable = cmd.stage == DatapathStage::GemEnable;
                configure_gem_port(session.fsl_mut(), onu.onu_index, 0, res.gem_id, enable)?;
                println!(
                    "[*] FSL staged gem_port: onu={} gem={:#x} enable={enable}",
                    cmd.onu_id, res.gem_id
                );
                Ok(serde_json::json!({
                    "status": "success",
                    "stage": if enable { "gem_enable" } else { "gem_disable" },
                    "onu_id": cmd.onu_id,
                    "gem_id": res.gem_id,
                }))
            }
            DatapathStage::AssignAllocPloamEnable | DatapathStage::AssignAllocPloamDisable => {
                let enable = cmd.stage == DatapathStage::AssignAllocPloamEnable;
                let seq = cmd.sequence.unwrap_or(1);
                let frame = assign_alloc_id_ploam(cmd.onu_id, res.alloc_id, enable, seq)?;
                for _ in 0..session.config().ploam_repeat {
                    session.send_ploam(&frame, &mut |delay| {
                        thread::sleep(delay);
                        Ok(())
                    })?;
                }
                println!(
                    "[*] FSL staged Assign_Alloc-ID PLOAM: onu={} alloc={:#x} enable={enable} seq={seq}",
                    cmd.onu_id, res.alloc_id
                );
                Ok(serde_json::json!({
                    "status": "success",
                    "stage": if enable { "assign_alloc_ploam_enable" } else { "assign_alloc_ploam_disable" },
                    "onu_id": cmd.onu_id,
                    "alloc_id": res.alloc_id,
                    "sequence": seq,
                }))
            }
            DatapathStage::GdbaEnable => {
                let mut alloc = GdbaAllocConfig::business_data(
                    tcont,
                    onu.onu_index,
                    session.dba_granularity_8x(),
                    onu.rate == OnuUpstreamRate::Rate2p5G,
                    cmd.fixed_bw,
                )?;
                if let Some(max_bw) = cmd.max_bw {
                    alloc.max_bw = max_bw;
                }
                if let Some(period) = cmd.ds_period {
                    alloc.ds_period = period;
                }
                if let Some(flag) = cmd.ds_flag {
                    alloc.ds_flag = flag;
                }
                if let Some(tail) = cmd.ds_tail_flag {
                    alloc.ds_tail_flag = tail;
                }
                session.configure_tcont_alloc(&alloc)?;
                println!(
                    "[*] FSL staged GDBA enable: onu={} tcont={tcont} fixed_bw={} max_bw={} alloc_type={} ds_period={} ds_flag={} ds_tail={}",
                    cmd.onu_id, alloc.fixed_bw, alloc.max_bw, alloc.alloc_type, alloc.ds_period, alloc.ds_flag, alloc.ds_tail_flag
                );
                Ok(serde_json::json!({
                    "status": "success",
                    "stage": "gdba_enable",
                    "onu_id": cmd.onu_id,
                    "tcont": tcont,
                    "fixed_bw": alloc.fixed_bw,
                    "max_bw": alloc.max_bw,
                    "alloc_type": alloc.alloc_type,
                    "ds_period": alloc.ds_period,
                    "ds_flag": alloc.ds_flag,
                    "ds_tail_flag": alloc.ds_tail_flag,
                }))
            }
            DatapathStage::GdbaDisable => {
                let disable_alloc = GdbaAllocConfig {
                    tcont,
                    onu_index: onu.onu_index,
                    enable: false,
                    fixed_bw: 0,
                    assured_bw: 0,
                    max_bw: 0,
                    alloc_type: 0,
                    priority: 0,
                    weight: 0,
                    ds_period: 0,
                    ds_flag: false,
                    ds_bw_flag: false,
                    ds_tail_flag: false,
                };
                session.configure_tcont_alloc(&disable_alloc)?;
                println!(
                    "[*] FSL staged GDBA disable: onu={} tcont={tcont}",
                    cmd.onu_id
                );
                Ok(serde_json::json!({
                    "status": "success",
                    "stage": "gdba_disable",
                    "onu_id": cmd.onu_id,
                    "tcont": tcont,
                }))
            }
            DatapathStage::Revoke => {
                configure_gem_port(session.fsl_mut(), onu.onu_index, 0, res.gem_id, false)?;
                let disable_alloc = GdbaAllocConfig {
                    tcont,
                    onu_index: onu.onu_index,
                    enable: false,
                    fixed_bw: 0,
                    assured_bw: 0,
                    max_bw: 0,
                    alloc_type: 0,
                    priority: 0,
                    weight: 0,
                    ds_period: 0,
                    ds_flag: false,
                    ds_bw_flag: false,
                    ds_tail_flag: false,
                };
                let _ = session.configure_tcont_alloc(&disable_alloc);
                println!(
                    "[*] FSL staged revoke: onu={} gem={:#x} tcont={tcont}",
                    cmd.onu_id, res.gem_id
                );
                Ok(serde_json::json!({
                    "status": "success",
                    "stage": "revoke",
                    "onu_id": cmd.onu_id,
                    "gem_id": res.gem_id,
                    "tcont": tcont,
                }))
            }
        }
    })
}

/// Read-only PON counter/PHY snapshot, bound to ONU identity and generation.
/// Served from the daemon's live FSL session so traffic-time sampling works;
/// it performs no write, reset or clear-on-read operation.
fn pon_stats(
    cmd: fttr_api::PonStatsRequest,
    states: &service::States,
) -> io::Result<serde_json::Value> {
    if !crate::controller::attribute_probe::identity_current(
        states,
        cmd.onu_id,
        &cmd.serial,
        cmd.generation,
    ) {
        return Err(io::Error::other(
            "ONU identity/registration generation mismatch",
        ));
    }
    with_session(|session| {
        let onu = session
            .onu(cmd.onu_id)
            .copied()
            .filter(|onu| onu.state == FslOnuState::Active)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ONU not active in FSL"))?;
        let snapshot = super::diagnostics::read_pon_stats(session.fsl_mut(), onu.onu_index)?;
        Ok(serde_json::json!({
            "status": "success",
            "stage": "pon_stats",
            "onu_id": cmd.onu_id,
            "snapshot": snapshot,
        }))
    })
}

/// Revokes only the ONU's business data GEM port and data T-CONT without
/// deactivating the ONU's O5 registration or OMCC management channel.
pub(crate) fn revoke_business(id: u8) -> io::Result<()> {
    let res = super::active_resources().onu(id)?;
    with_session(|session| {
        let Some(onu) = session.onu(id).copied() else {
            return Ok(());
        };
        configure_gem_port(session.fsl_mut(), onu.onu_index, 0, res.gem_id, false)?;
        let max_onus = session.config().init.max_onu_id.min(128) as u8;
        if let Ok(tcont) = alloc_to_tcont(res.alloc_id, 0, max_onus, 0) {
            let disable_alloc = GdbaAllocConfig {
                tcont,
                onu_index: onu.onu_index,
                enable: false,
                fixed_bw: 0,
                assured_bw: 0,
                max_bw: 0,
                alloc_type: 0,
                priority: 0,
                weight: 0,
                ds_period: 0,
                ds_flag: false,
                ds_bw_flag: false,
                ds_tail_flag: false,
            };
            let _ = session.configure_tcont_alloc(&disable_alloc);
        }
        Ok(())
    })
}

struct RuntimeOptics<'a> {
    ready: bool,
    services: &'a mut service::Services,
    engine: &'a Mutex<PonEngine>,
    stopping: &'a AtomicBool,
    #[cfg(feature = "npu")]
    npu: Option<&'a crate::npu::ServiceHandle>,
}

fn calibration_checkpoint(stopping: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if stopping.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "controller is stopping",
        ))
    } else if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "calibration programming deadline",
        ))
    } else {
        Ok(())
    }
}

impl OpticalBackend for RuntimeOptics<'_> {
    fn validate_calibration(&self) -> io::Result<()> {
        super::calibration::load_factory().map(|_| ())
    }

    fn apply_calibration(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        super::calibration::apply_factory(|| calibration_checkpoint(self.stopping, deadline))
    }

    fn calibration_requires_disabled_tx(&self) -> bool {
        true
    }

    fn save_settings(&mut self, settings: fttr_api::OpticalSettings) -> io::Result<()> {
        crate::optical::save(settings)
    }

    fn mac_tx_allowed(&self) -> bool {
        crate::controller::pon_mac::snapshot().tx_enabled == Some(true)
    }

    fn reconcile_controls(&mut self, master_enabled: bool) -> io::Result<()> {
        let settings = crate::optical::settings();
        let requested =
            master_enabled && settings.enabled && !self.stopping.load(Ordering::Acquire);
        let calibrated = !requested || super::calibration::info().application.applied;
        let enabled = requested && calibrated;
        crate::controller::pon_mac::reconcile(
            (
                enabled && settings.downstream_tx_enabled,
                enabled && settings.fec_enabled,
            ),
            || self.quiesce(),
            |tx, fec| with_session(|session| session.fsl_mut().set_downstream(tx, fec)),
        )?;
        if !calibrated {
            return Err(io::Error::other(
                "factory calibration must be successfully applied before enabling downstream",
            ));
        }
        Ok(())
    }

    fn ready(&self) -> bool {
        self.ready
    }

    fn cleanup_pending(&self) -> bool {
        runtime_state().cleanup_pending.load(Ordering::Acquire)
    }

    fn quiesce(&mut self) -> io::Result<()> {
        runtime_state()
            .cleanup_pending
            .store(true, Ordering::Release);
        #[cfg(feature = "npu")]
        if let Some(npu) = self.npu {
            npu.withdraw();
        }
        self.services.stop_all();
        let mut errors = Vec::new();
        // Software activation evidence must be withdrawn even if SPI inventory
        // or hardware deactivation fails. A frozen management queue is not O5.
        match self.engine.lock() {
            Ok(mut engine) => {
                let ids: Vec<_> = engine.active_onus.iter().map(|onu| onu.onu_id).collect();
                for id in ids {
                    engine.mark_offline(id);
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
        match runtime_state().omci_rx.lock() {
            Ok(mut queues) => queues.clear(),
            Err(error) => errors.push(error.to_string()),
        }
        let active: Vec<u8> =
            match with_session(|session| Ok(session.active_onus().map(|onu| onu.onu_id).collect()))
            {
                Ok(active) => active,
                Err(error) => {
                    errors.push(error.to_string());
                    Vec::new()
                }
            };
        for id in active {
            if let Err(error) =
                with_session(|session| deactivate_session_onu(session, id).map(|_| ()))
            {
                errors.push(format!("ONU {id} deactivate: {error}"));
            }
        }
        if let Err(error) = super::vport::bring_all_ports_down() {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            runtime_state()
                .cleanup_pending
                .store(false, Ordering::Release);
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }

    fn set_tx(&mut self, enabled: bool) -> io::Result<()> {
        if enabled && self.cleanup_pending() {
            return Err(io::Error::other("optical cleanup is incomplete"));
        }
        if enabled && self.stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "controller is stopping",
            ));
        }
        if enabled && !super::calibration::info().application.applied {
            return Err(io::Error::other(
                "factory calibration must be successfully applied before enabling downstream",
            ));
        }
        let settings = crate::optical::settings();
        let result = with_session(|session| {
            session.fsl_mut().set_downstream(
                enabled && settings.downstream_tx_enabled,
                enabled && settings.fec_enabled,
            )
        });
        let error = result.as_ref().err().map(ToString::to_string);
        crate::controller::pon_mac::record(result);
        error.map_or(Ok(()), |error| Err(io::Error::other(error)))
    }

    fn read_tx(&mut self) -> io::Result<bool> {
        // Same semantics as the HM2004 backend: the actual register value, not
        // the software's committed state. A sticky register after a failed
        // recovery write must be visible to the owner instead of being masked;
        // this is not a claim of measured light or a BOSA reset readback.
        with_session(|session| {
            session
                .fsl_mut()
                .pon_mac_readback()
                .and_then(readback_tx_enabled)
        })
    }

    fn desired(&self) -> io::Result<bool> {
        crate::optical::load_enabled()
    }

    fn save(&mut self, enabled: bool) -> io::Result<()> {
        crate::optical::save_enabled(enabled)
    }
}

fn initialize_hardware(
    engine: &Mutex<PonEngine>,
    stopping: &AtomicBool,
) -> io::Result<OpticalState> {
    let loaded = crate::provisioning::load_config().map_err(io::Error::other)?;
    super::active_resources()
        .validate_pins(&loaded.onu_pins)
        .map_err(io::Error::other)?;
    let pins = loaded.onu_pins;
    let config = OltSessionConfig {
        nni_eth_tpid: super::initialization::defaults::IEEE_8021AD_TPID,
        init: super::initialization::InitConfig {
            max_onu_id: super::active_resources().onu_ids.len() as u16,
            ..Default::default()
        },
        ..OltSessionConfig::default()
    };
    super::vport::prepare_nni_and_ports(NNI_INTERFACE, config.nni_gem_port_offset, stopping)?;
    let dev = Device::open(Path::new(FSL_DEVICE_PATH))?;
    let mut session = FslOltSession::new(dev, config);
    session.set_onu_pins(pins.clone());
    // Report the session-enforced set as the active pinning state: provisioning
    // seeds the engine with the raw desired configuration, and a degraded set
    // must show up as desired != active (pending_restart), not as enforced.
    if let Ok(mut engine) = engine.lock() {
        engine.onu_pins = pins;
    } else {
        // A poisoned engine lock leaves the status view on its seeded pin set
        // while the session enforces the degraded set; say so instead of
        // silently diverging.
        eprintln!("[-] FSL engine lock poisoned; status keeps the seeded pin set");
    }
    let initialized: io::Result<_> = (|| {
        session.initialize(&mut |delay| {
            if stopping.load(Ordering::Acquire) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopping"));
            }
            thread::sleep(delay);
            Ok(())
        })?;
        // Stock `apply_gemport_defaults` pre-maps GEM 256..271 to ONU 0 and enables
        // WMCI on GEM 270. Clear our managed business GEM slots so each ONU's GEM
        // is bound strictly to its own `onu_index` only after provisioning succeeds.
        for entry in super::active_resources().allocations {
            configure_gem_port(session.fsl_mut(), 0, 0, entry.gem_id, false)?;
            set_gem_wmci_aes(session.fsl_mut(), entry.gem_id, false, false)?;
        }
        session.fsl_mut().set_downstream(false, false)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        super::calibration::apply_factory(|| calibration_checkpoint(stopping, deadline))?;
        let desired = crate::optical::load_enabled().unwrap_or(false);
        let settings = crate::optical::settings();
        // Acquire the publication slot before enabling, so publication cannot fail
        // after a successful on write. No global session exists during preparation.
        let slot = runtime_state()
            .session
            .lock()
            .map_err(|_| io::Error::other("FSL runtime session lock poisoned"))?;
        calibration_checkpoint(stopping, deadline)?;
        let readback = session.fsl_mut().set_downstream(
            desired && settings.downstream_tx_enabled,
            desired && settings.fec_enabled,
        )?;
        crate::controller::pon_mac::record(Ok(readback));
        Ok((desired, slot))
    })();
    let (desired, mut slot) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            // Initialization may already have released reset or written TX.
            // Retain the local SPI session until an off write/readback is tried.
            let disabled = session.fsl_mut().set_downstream(false, false);
            let cleanup = disabled.as_ref().err().map(ToString::to_string);
            crate::controller::pon_mac::record(disabled);
            return Err(io::Error::other(match cleanup {
                Some(cleanup) => format!("{error}; initialization shutdown failed: {cleanup}"),
                None => error.to_string(),
            }));
        }
    };
    *slot = Some(session);
    Ok(if desired {
        OpticalState::Enabled
    } else {
        OpticalState::Disabled
    })
}

/// Whether another full initialization attempt is worth running. Each attempt
/// rebuilds vports and reopens the device from scratch, so retrying is only
/// meaningful before service shutdown and only after a failure.
fn init_should_retry(initial: &io::Result<OpticalState>, stopping: bool) -> bool {
    match initial {
        // A rejected platform invariant (e.g. the offset=0 fail-fast) is a
        // configuration mismatch: retrying cannot fix it and only burns
        // bounded hardware reset cycles before the supervised restart.
        Err(error) if error.kind() == io::ErrorKind::Unsupported => false,
        Err(_) => !stopping,
        Ok(_) => false,
    }
}

/// Register truth for optical replies; the committed master flag stays
/// owner-internal and must never mask a sticky hardware bit.
fn readback_tx_enabled(readback: crate::platform::resources::PonMacReadback) -> io::Result<bool> {
    readback
        .tx_enabled
        .ok_or_else(|| io::Error::other("FSL PON-MAC readback lost the TX bit"))
}

/// Returns the fault message for a permanent initialization failure, or None
/// when initialization succeeded or was interrupted by a service shutdown
/// (procd stop is not a fault).
fn init_fault_message(initial: &io::Result<OpticalState>, stopping: bool) -> Option<String> {
    match initial {
        Ok(_) => None,
        Err(_) if stopping => None,
        Err(error) => Some(format!(
            "FSL hardware initialization failed permanently: {error}"
        )),
    }
}

/// Bounded backoff between full initialization attempts; wakes early on stop.
fn init_retry_backoff(attempt: u32, stopping: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(u64::from(2 * attempt));
    while Instant::now() < deadline && !stopping.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn status_snapshot(
    optics: &OpticalClient,
    pon_engine: &Mutex<PonEngine>,
    devices: &Mutex<Vec<SubDevice>>,
    services: &service::States,
    provisioning: fttr_api::SyncStatusReply,
) -> Result<fttr_api::StatusReply, i32> {
    let ready = optics.hardware_ready.load(Ordering::Acquire);
    // Same discipline as data_path: never wait for the session lock in the
    // status path — a busy fsl-loop transaction (PLOAM/OMCI bursts, deactivate)
    // would otherwise stall the single IPC STATUS worker. An unavailable
    // sample beats blocking; committed state remains visible via pon_mac.
    let (tx_enabled, onu_us_fec) = runtime_state()
        .session
        .try_lock()
        .ok()
        .and_then(|mut guard| {
            guard.as_mut().map(|session| {
                (
                    session.fsl_mut().is_tx_enabled().ok(),
                    session.read_onu_us_fec_states().unwrap_or_default(),
                )
            })
        })
        .unwrap_or((None, BTreeMap::new()));
    let calibration = super::calibration::info().application;
    let (measurements, measurement_evidence) = super::transceiver::snapshot();
    let devices = devices.lock().map_err(|_| UBUS_ERROR_INTERNAL)?;
    let engine = pon_engine.lock().map_err(|_| UBUS_ERROR_INTERNAL)?;
    let phy_rate_uplink_gbps = if engine.active_onus.iter().any(|onu| {
        onu.state == crate::pon_engine::ploam::OnuState::O5Operation
            && onu.upstream_phy_rate_gbps == Some(2.488)
    }) {
        2.488
    } else {
        1.244
    };
    Ok(crate::platform::status::snapshot(
        &engine,
        services,
        (&devices, provisioning),
        crate::platform::status::Hardware {
            switch_port_count: super::active_resources().onu_ids.len() as u8,
            // initialize_hardware publishes the session-enforced pin set to the
            // engine, so the pinning report distinguishes the enforced set from
            // the desired configuration (pending_restart on divergence) instead
            // of rendering an unenforced desired state as active.
            supports_pinning: true,
            ranging_source: |_| "FSL upstream ranging time / calculated EqD".into(),
            has_pon_mac_fec: true,
            onu_us_fec,
            data_path,
            fpga_status: if ready { "ready" } else { "not_ready" }.into(),
            pon_mac: Some(crate::controller::pon_mac::snapshot()),
            optical_transceiver: fttr_api::OpticalTransceiverStatus {
                calibration_sha256: calibration.sha256,
                calibration_applied: calibration.applied,
                model: "UX3326S1".into(),
                wavelength_tx_nm: 1490,
                wavelength_rx_nm: 1310,
                phy_rate_downlink_gbps: 2.488,
                phy_rate_uplink_gbps,
                tx_power_dbm: measurements[3],
                laser_bias_current_ma: measurements[2],
                temperature_celsius: measurements[0],
                vcc_voltage: measurements[1],
                tx_enabled,
                measurements: measurement_evidence,
            },
            npu: {
                #[cfg(feature = "npu")]
                {
                    Some(crate::npu::status_for_platform(
                        crate::platform::adapter::PlatformId::FiberhomeAn4475G2,
                    ))
                }
                #[cfg(not(feature = "npu"))]
                {
                    None
                }
            },
        },
    ))
}

struct PortCounters {
    at: Instant,
    tx: Option<u64>,
    rx: Option<u64>,
}

fn read_sysfs_counter(path: &Path) -> (Option<u64>, fttr_api::MeasurementEvidence) {
    let source = path.display().to_string();
    let unit = if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("packets"))
    {
        "packets"
    } else {
        "bytes"
    };
    match fs::read_to_string(path) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(value) => (
                Some(value),
                fttr_api::MeasurementEvidence::observed(source, unit, raw.trim()),
            ),
            Err(error) => (
                None,
                fttr_api::MeasurementEvidence::unavailable(
                    source,
                    unit,
                    format!("invalid_counter: {error}"),
                ),
            ),
        },
        Err(error) => (
            None,
            fttr_api::MeasurementEvidence::unavailable(
                source,
                unit,
                format!("read_failed: {error}"),
            ),
        ),
    }
}

fn counter_rate_kbps(
    current: Option<u64>,
    previous: Option<u64>,
    elapsed: Duration,
) -> Option<f64> {
    if elapsed < Duration::from_secs(1) {
        return None;
    }
    let delta = current?.checked_sub(previous?)?;
    Some((delta as f64) * 8.0 / elapsed.as_secs_f64() / 1000.0)
}

fn data_path(
    id: u8,
    registration: &str,
    inventory: Option<fttr_api::OnuOmciInventory>,
) -> fttr_api::DataPathStatus {
    static SAMPLES: OnceLock<Mutex<BTreeMap<String, PortCounters>>> = OnceLock::new();
    let interface = super::active_resources()
        .interface(id)
        .unwrap_or("unmapped")
        .to_owned();
    let base = Path::new("/sys/class/net")
        .join(&interface)
        .join("statistics");
    let mut measurements = fttr_api::Measurements::new();
    measurements.insert(
        "interface_scope".into(),
        fttr_api::MeasurementEvidence::observed("FSL 802.1ad VPORT", "scope", "per_onu_vport"),
    );
    let (tx_bytes, tx_ev) = read_sysfs_counter(&base.join("tx_bytes"));
    let (rx_bytes, rx_ev) = read_sysfs_counter(&base.join("rx_bytes"));
    let (_tx_packets, tx_pkts_ev) = read_sysfs_counter(&base.join("tx_packets"));
    let (_rx_packets, rx_pkts_ev) = read_sysfs_counter(&base.join("rx_packets"));
    measurements.insert("tx_bytes".into(), tx_ev);
    measurements.insert("rx_bytes".into(), rx_ev);
    measurements.insert("tx_packets".into(), tx_pkts_ev);
    measurements.insert("rx_packets".into(), rx_pkts_ev);

    let mut current_tx_kbps = None;
    let mut current_rx_kbps = None;
    if let Ok(mut samples) = SAMPLES.get_or_init(|| Mutex::new(BTreeMap::new())).lock() {
        let key = format!("{interface}:{registration}");
        let previous = samples.get(&key);
        if let Some(previous) = previous {
            current_tx_kbps = counter_rate_kbps(tx_bytes, previous.tx, previous.at.elapsed());
            current_rx_kbps = counter_rate_kbps(rx_bytes, previous.rx, previous.at.elapsed());
        }
        if previous.is_none_or(|sample| sample.at.elapsed() >= Duration::from_secs(1)) {
            samples.insert(
                key,
                PortCounters {
                    at: Instant::now(),
                    tx: tx_bytes,
                    rx: rx_bytes,
                },
            );
        }
    }
    for (name, value) in [
        ("current_tx_kbps", current_tx_kbps),
        ("current_rx_kbps", current_rx_kbps),
    ] {
        let evidence = match value {
            Some(value) => fttr_api::MeasurementEvidence::observed(
                format!("{}/counter_delta", base.display()),
                "kbit/s",
                value.to_string(),
            ),
            None => fttr_api::MeasurementEvidence::unavailable(
                format!("{}/counter_delta", base.display()),
                "kbit/s",
                "requires_two_valid_monotonic_samples_at_least_one_second_apart",
            ),
        };
        measurements.insert(name.into(), evidence);
    }

    measurements.insert(
        "nni_expected_tpid".into(),
        fttr_api::MeasurementEvidence::observed(
            "FSL runtime 802.1ad VPORT constant; not readback",
            "hex",
            format!(
                "0x{:04x}",
                super::initialization::defaults::IEEE_8021AD_TPID
            ),
        ),
    );
    measurements.insert(
        "nni_expected_gem_offset".into(),
        fttr_api::MeasurementEvidence::observed(
            "FSL initialization constant; not readback",
            "gem_port_id",
            super::initialization::defaults::DEFAULT_NNI_GEMPORT_OFFSET.to_string(),
        ),
    );
    // The session is also used by OMCI and the activation worker. Never wait
    // for it while status holds the engine/device locks: an unavailable sample
    // is preferable to blocking or inverting the hardware lock order.
    use super::initialization::defaults::{
        NNI_ETH_TPID_CFG, NNI_GEMPORT_OFFSET_CFG, VLAN_TYPE_CFG,
    };
    let register_values = runtime_state()
        .session
        .try_lock()
        .ok()
        .and_then(|mut guard| {
            guard.as_mut().map(|session| {
                [NNI_ETH_TPID_CFG, VLAN_TYPE_CFG, NNI_GEMPORT_OFFSET_CFG]
                    .map(|address| session.fsl_mut().read_register(address).ok())
            })
        });
    for (index, name, address, mask, unit) in [
        (0, "nni_tpid_readback", NNI_ETH_TPID_CFG, 0xffff, "hex"),
        (1, "nni_vlan_type_readback", VLAN_TYPE_CFG, 0xffff, "hex"),
        (
            2,
            "nni_gem_offset_readback",
            NNI_GEMPORT_OFFSET_CFG,
            0x0fff,
            "gem_port_id",
        ),
    ] {
        let source = format!("shared FSL register {address:#x}; readback");
        let evidence = match register_values.and_then(|values| values[index]) {
            Some(value) if unit == "hex" => fttr_api::MeasurementEvidence::observed(
                source,
                unit,
                format!("0x{:04x}", value & mask),
            ),
            Some(value) => {
                fttr_api::MeasurementEvidence::observed(source, unit, (value & mask).to_string())
            }
            None => fttr_api::MeasurementEvidence::unavailable(
                source,
                unit,
                "session_busy_or_register_unavailable",
            ),
        };
        measurements.insert(name.into(), evidence);
    }

    let olt_channel_gem = super::active_resources().onu(id).ok().map(|r| r.gem_id);
    let gem_ports: Option<Vec<u16>> = inventory
        .as_ref()
        .and_then(|v| v.gem_ports.iter().map(|p| p.port_id).collect());
    if let Some(ref inv) = inventory {
        for field in ["gem_ports", "onu_inventory"] {
            let mut ev = fttr_api::MeasurementEvidence::observed(
                "fresh ONU OMCI MIB upload; not OLT forwarding proof",
                "ONU configuration",
                format!(
                    "{} GEM CTPs; {} VLAN filters; {} VLAN operation tables",
                    inv.gem_ports.len(),
                    inv.vlan_filters.len(),
                    inv.vlan_operations.len()
                ),
            );
            ev.age_seconds = Some(inv.age_seconds);
            measurements.insert(field.into(), ev);
        }
    }
    measurements.insert(
        "forwarding_proof".into(),
        fttr_api::MeasurementEvidence::unavailable(
            format!("FSL VPORT {interface}"),
            "forwarding_proof",
            "current_bidirectional_forwarding_not_verified",
        ),
    );
    let mut status = fttr_api::DataPathStatus {
        interface,
        gem_ports,
        vlan_id: None,
        onu_inventory: inventory,
        tx_bytes,
        rx_bytes,
        current_tx_kbps,
        current_rx_kbps,
        olt_channel_gem,
        forwarding_verified: None,
        offload: Default::default(),
        measurements,
    };
    collect_direction_offload(
        &mut status,
        Path::new("/sys/module/airoha_eth/parameters/fttr_offload_directions"),
        Path::new("/sys/kernel/debug/ppe/config"),
    );
    status
}

fn collect_direction_offload(
    status: &mut fttr_api::DataPathStatus,
    directions_path: &Path,
    ppe_path: &Path,
) {
    let directions = fs::read_to_string(directions_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u8>().ok())
        .filter(|mask| *mask <= 7);
    let attached = fs::read_to_string(ppe_path).ok().and_then(|raw| {
        raw.lines().find_map(|line| match line.split_once(':') {
            Some(("npu_attached", value)) => match value.trim() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            },
            _ => None,
        })
    });
    status.offload.routed_enabled = directions.map(|mask| mask & 3 != 0);
    status.offload.bridge_enabled = directions.map(|mask| mask & 4 != 0);
    status.offload.enabled = directions.map(|mask| mask != 0);
    status.offload.engine_attached = attached;
    for (field, value, path) in [
        (
            "offload_routed_enabled",
            status.offload.routed_enabled,
            directions_path,
        ),
        (
            "offload_bridge_enabled",
            status.offload.bridge_enabled,
            directions_path,
        ),
        ("offload_engine_attached", attached, ppe_path),
    ] {
        let evidence = match value {
            Some(value) => fttr_api::MeasurementEvidence::observed(
                path.display().to_string(),
                "boolean",
                value.to_string(),
            ),
            None => fttr_api::MeasurementEvidence::unavailable(
                path.display().to_string(),
                "boolean",
                "unreadable_or_invalid_offload_state",
            ),
        };
        status.measurements.insert(field.into(), evidence);
    }
    status.offload.forwarding_verified = None;
    status.measurements.insert(
        "offload_forwarding_proof".into(),
        fttr_api::MeasurementEvidence::unavailable(
            "per-flow PPE hardware hit evidence",
            "forwarding_proof",
            "not_verified; direction_gate_and_engine_attachment_do_not_prove_per_onu_hardware_forwarding",
        ),
    );
}

fn ipc_handler(
    devices: Arc<Mutex<Vec<SubDevice>>>,
    pon_engine: Arc<Mutex<PonEngine>>,
    services: service::States,
    optics: OpticalClient,
) -> impl Fn(UnixStream, &str) + Send + Sync + 'static {
    move |mut stream, line| {
        if line == "BRIDGE SETUP" {
            let response = match network::setup_bridge(super::active_resources(), "br-lan") {
                Ok(()) => serde_json::json!({"status": "success"}),
                Err(error) => serde_json::json!({"status": "error", "error": error.to_string()}),
            };
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("HL202 ") {
            #[cfg(feature = "device-hl202")]
            crate::controller::hl202::start(
                stream,
                request.to_string(),
                pon_engine.clone(),
                services.clone(),
            );
            #[cfg(not(feature = "device-hl202"))]
            {
                let _ = request;
                let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"adapter_disabled\",\"message\":\"HL202 adapter not compiled\"}}");
            }
        } else if let Some(request) = line.strip_prefix("PROVISIONING ") {
            let _ = stream.write_all(crate::provisioning::control(request).as_bytes());
        } else if let Some(request) = line.strip_prefix("FEC_PM ") {
            let result = serde_json::from_str::<crate::controller::fec::Command>(request)
                .map_err(std::io::Error::other)
                .and_then(|command| crate::controller::fec::control(command, &services));
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("US_FEC ") {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct UsFecCommand {
                onu_id: u8,
                #[serde(default)]
                enable: Option<bool>,
            }
            let response = match serde_json::from_str::<UsFecCommand>(request)
                .map_err(std::io::Error::other)
                .and_then(|cmd| {
                    with_session(|session| {
                        let b_all_set_thd = if let Some(enable) = cmd.enable {
                            session.set_onu_us_fec_and_recalc(cmd.onu_id, enable)?
                        } else {
                            None
                        };
                        let states = session.read_onu_us_fec_states()?;
                        let enabled = states.get(&cmd.onu_id).copied().ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "unknown ONU-ID in OLT session")
                        })?;
                        Ok(serde_json::json!({
                            "status": "success",
                            "onu_id": cmd.onu_id,
                            "upstream_fec_enabled": enabled,
                            "b_all_set_thd": b_all_set_thd,
                        }))
                    })
                }) {
                Ok(value) => value,
                Err(error) => serde_json::json!({"status": "error", "error": error.to_string()}),
            };
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("OMCI_SNAPSHOT ") {
            let result =
                serde_json::from_str::<crate::controller::attribute_probe::Command>(request)
                    .map_err(std::io::Error::other)
                    .and_then(|command| {
                        crate::controller::attribute_probe::snapshot(
                            optics.platform,
                            command,
                            &services,
                            &stream,
                        )
                    });
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("OMCI_SET_ONCE ") {
            let result =
                serde_json::from_str::<crate::controller::attribute_probe::Command>(request)
                    .map_err(std::io::Error::other)
                    .and_then(|command| {
                        crate::controller::attribute_probe::set_once(
                            optics.platform,
                            command,
                            &services,
                            &stream,
                        )
                    });
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("OMCI_EVTO_RULE ") {
            let result =
                serde_json::from_str::<crate::controller::attribute_probe::EvtoRuleCommand>(
                    request,
                )
                .map_err(std::io::Error::other)
                .and_then(|command| {
                    crate::controller::attribute_probe::evto_rule(
                        optics.platform,
                        command,
                        &services,
                        &stream,
                    )
                });
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("PON_STATS ") {
            let result = serde_json::from_str::<fttr_api::PonStatsRequest>(request)
                .map_err(std::io::Error::other)
                .and_then(|command| pon_stats(command, &services));
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("DATAPATH_STAGE ") {
            let result = serde_json::from_str::<DatapathStageCommand>(request)
                .map_err(std::io::Error::other)
                .and_then(|command| datapath_stage(command, &services));
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("OMCI_ATTRIBUTE ") {
            let result =
                serde_json::from_str::<crate::controller::attribute_probe::Command>(request)
                    .map_err(std::io::Error::other)
                    .and_then(|command| {
                        crate::controller::attribute_probe::control(command, &services)
                    });
            let response =
                result.unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let _ = serde_json::to_writer(&mut stream, &response);
            let _ = stream.write_all(b"\n");
        } else if let Some(request) = line.strip_prefix("MIB ") {
            let result = serde_json::from_str::<crate::controller::inventory::MibCommand>(request)
                .map_err(std::io::Error::other)
                .and_then(|command| {
                    let progress = command
                        .progress_id
                        .as_deref()
                        .map(crate::controller::inventory::MibProgressReporter::new)
                        .transpose()?;
                    crate::controller::inventory::control(
                        optics.platform,
                        command.onu_id,
                        command.tables,
                        progress,
                        &services,
                    )
                });
            match result {
                Ok(report) => {
                    let _ = serde_json::to_writer(&mut stream, &report);
                }
                Err(error) => {
                    let _ = serde_json::to_writer(
                        &mut stream,
                        &serde_json::json!({"schema_version": 1, "error": {"code": format!("{:?}", error.kind()), "message": error.to_string()}}),
                    );
                }
            }
            let _ = stream.write_all(b"\n");
        } else if let Some(_request) = line.strip_prefix("ACS ") {
            #[cfg(feature = "acs")]
            let response = crate::acs::control(_request);
            #[cfg(not(feature = "acs"))]
            let response =
                "{\"error\":\"ACS adapter not compiled (enable Cargo feature acs)\"}".to_string();
            let _ = stream.write_all(response.as_bytes());
        } else if line == "CAPABILITIES" {
            let _ = writeln!(stream, "{}", crate::capabilities::json());
        } else if line == "STATUS" {
            let reply = serde_json::from_value(crate::provisioning::snapshot())
                .map_err(|_| UBUS_ERROR_INTERNAL)
                .and_then(|provisioning| {
                    status_snapshot(&optics, &pon_engine, &devices, &services, provisioning)
                });
            match reply {
                Ok(status) => {
                    let _ = serde_json::to_writer(&mut stream, &status);
                }
                Err(_) => {
                    let _ = serde_json::to_writer(
                        &mut stream,
                        &serde_json::json!({"status":"error","error":"status unavailable"}),
                    );
                }
            }
            let _ = stream.write_all(b"\n");
        } else if line == "DEVICES" {
            let list = devices.lock().map(|d| d.clone()).unwrap_or_default();
            let _ = serde_json::to_writer(
                &mut stream,
                &serde_json::json!({"status": "success", "devices": list}),
            );
            let _ = stream.write_all(b"\n");
        } else if let Some(cmd) = line.strip_prefix("OPTICAL ") {
            let req = match cmd.trim() {
                "ON" => Ok(OpticalCommand::On),
                "OFF" => Ok(OpticalCommand::Off),
                "STATUS" => Ok(OpticalCommand::Status),
                "RELOAD" => Ok(OpticalCommand::Reload),
                "REINITIALIZE" => Ok(OpticalCommand::Reinitialize),
                "SHUTDOWN" => Ok(OpticalCommand::Shutdown),
                other => Err(format!("unknown optical command: {other}")),
            };
            match req {
                Ok(cmd) => match optics.request(cmd) {
                    Ok(reply) => {
                        let _ = serde_json::to_writer(&mut stream, &reply);
                    }
                    Err(e) => {
                        let _ = serde_json::to_writer(
                            &mut stream,
                            &serde_json::json!({"status": "error", "error": e.to_string()}),
                        );
                    }
                },
                Err(msg) => {
                    let _ = serde_json::to_writer(
                        &mut stream,
                        &serde_json::json!({"status": "error", "error": msg}),
                    );
                }
            }
            let _ = stream.write_all(b"\n");
        } else {
            let _ = &services;
            let _ = serde_json::to_writer(
                &mut stream,
                &serde_json::json!({"status": "error", "error": "unsupported IPC command"}),
            );
            let _ = stream.write_all(b"\n");
        }
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn shared_ipc_routes_are_available_on_fiberhome() {
        let platform = PlatformContext::test_context();
        let devices = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::new(Mutex::new(PonEngine::new(platform.resources())));
        let services = Arc::new(Mutex::new(BTreeMap::new()));
        let (requests, _receiver) = mpsc::sync_channel(2);
        let handler = ipc_handler(
            devices,
            engine,
            services,
            OpticalClient {
                platform,
                requests,
                hardware_ready: Arc::new(AtomicBool::new(false)),
                stopping: Arc::new(AtomicBool::new(false)),
            },
        );
        for request in [
            "CAPABILITIES",
            "ACS invalid",
            "PROVISIONING invalid",
            "FEC_PM invalid",
            "OMCI_SNAPSHOT invalid",
            "OMCI_SET_ONCE invalid",
            "OMCI_EVTO_RULE invalid",
            "PON_STATS invalid",
            "DATAPATH_STAGE invalid",
            "OMCI_ATTRIBUTE invalid",
            "MIB invalid",
        ] {
            let (mut reader, writer) = UnixStream::pair().unwrap();
            handler(writer, request);
            let mut response = String::new();
            reader.read_to_string(&mut response).unwrap();
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_ne!(
                value.get("error").and_then(|v| v.as_str()),
                Some("unsupported IPC command"),
                "{request}"
            );
        }
    }
}

pub fn run(platform: PlatformContext) {
    crate::runtime::block_on(run_async(platform));
}

async fn run_async(platform: PlatformContext) {
    println!("[*] Starting FiberHome AN4475-G2 FTTR Master Controller Daemon (Rust fttrd)...");
    let _owner = crate::optical::claim_owner().unwrap_or_else(|error| {
        eprintln!("[-] Optical ownership failed: {error}");
        std::process::exit(1);
    });
    let lifetime = tokio::select! {
        result = lifetime::Lifetime::start() => result.unwrap_or_else(|error| {
            eprintln!("[-] MQTT startup failed: {error}");
            std::process::exit(1);
        }),
        _ = crate::runtime::termination() => return,
    };
    #[cfg(feature = "npu")]
    let npu = crate::npu::Service::start(platform);
    #[cfg(feature = "npu")]
    let npu_handle = npu.handle();
    #[cfg(feature = "acs")]
    crate::acs::start_from_env();

    let _ = fs::create_dir_all(ipc::SUBDEV_DIR);
    let devices: Arc<Mutex<Vec<SubDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let pon_engine = Arc::new(Mutex::new(PonEngine::new(platform.resources())));
    service::init_pon_engine(pon_engine.clone());
    crate::provisioning::start(pon_engine.clone());

    let hardware_ready = Arc::new(AtomicBool::new(false));
    let stopping = Arc::new(AtomicBool::new(false));
    let (optical_requests, optical_receiver) = mpsc::sync_channel(2);
    let shutdown_client = OpticalClient {
        platform,
        requests: optical_requests.clone(),
        hardware_ready: hardware_ready.clone(),
        stopping: stopping.clone(),
    };
    let service_states: service::States = Arc::new(Mutex::new(BTreeMap::new()));

    match ipc::create_socket_listener() {
        Ok(listener) => {
            ipc_server::start(
                listener,
                ipc_handler(
                    devices.clone(),
                    pon_engine.clone(),
                    service_states.clone(),
                    OpticalClient {
                        platform,
                        requests: optical_requests.clone(),
                        hardware_ready: hardware_ready.clone(),
                        stopping: stopping.clone(),
                    },
                ),
            );
            println!("[+] IPC socket server listening on {}", ipc::SOCKET_PATH);
        }
        Err(error) => {
            // fttrctl is unreachable without the socket; let procd rebuild the
            // process instead of running an unmanageable daemon.
            eprintln!("[-] IPC socket listener unavailable: {error}");
            std::process::exit(1);
        }
    }

    let _ubus_thread = super::ubus::start(
        OpticalClient {
            platform,
            requests: optical_requests.clone(),
            hardware_ready: hardware_ready.clone(),
            stopping: stopping.clone(),
        },
        devices.clone(),
        pon_engine.clone(),
        service_states.clone(),
        stopping.clone(),
    );

    let pon_engine_loop = pon_engine.clone();
    let states_loop = service_states.clone();
    let stopping_loop = stopping.clone();
    let ready_loop = hardware_ready.clone();
    let (fault_sender, fault_receiver) = tokio::sync::oneshot::channel::<String>();
    // send() consumes the sender; the permanent-initialization, session-loss and
    // needs-reconciliation paths must each be able to fire it exactly once.
    let mut fault_sender = Some(fault_sender);
    let fsl_thread = thread::Builder::new()
        .name("fsl-loop".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            // Bounded full retries absorb transient device/config timing without
            // burning procd's finite respawn budget; persistent failure escalates
            // through the fault channel so procd creates a fresh process.
            const INIT_ATTEMPTS: u32 = 3;
            let mut initial = initialize_hardware(&pon_engine_loop, &stopping_loop);
            for attempt in 1..INIT_ATTEMPTS {
                if !init_should_retry(&initial, stopping_loop.load(Ordering::Acquire)) {
                    break;
                }
                if let Err(error) = &initial {
                    eprintln!(
                        "[-] FSL hardware initialization failed (retry {attempt}/{}): {error}",
                        INIT_ATTEMPTS - 1
                    );
                }
                init_retry_backoff(attempt, &stopping_loop);
                initial = initialize_hardware(&pon_engine_loop, &stopping_loop);
            }
            let permanent_fault =
                init_fault_message(&initial, stopping_loop.load(Ordering::Acquire));
            let (ready, state) = match initial {
                Ok(state) => (true, state),
                Err(_) => (false, OpticalState::Unknown),
            };
            ready_loop.store(ready, Ordering::Release);
            if let Some(message) = permanent_fault.clone() {
                // No session was ever published, so in-process recovery is
                // impossible: request a supervised restart instead of spinning
                // as a non-functional daemon. The flag itself must survive so
                // the loop below stays skipped until the process exits.
                if let Some(sender) = fault_sender.take() {
                    let _ = sender.send(message);
                }
            }
            let mut services = service::Services::new(platform, states_loop);
            let mut optics = OpticalActor { state };
            let mut observation_start = Instant::now();
            let mut last_timeout_check = Instant::now();
            let mut observation_first = true;
            let mut observation_counts = (0usize, 0usize, 0usize, 0usize);
            let mut receive_snapshots_remaining = if crate::startup_config::get().rx_snapshot {
                3u8
            } else {
                0
            };
            let mut mac_poll = crate::controller::pon_mac::Poll::default();
            let mut next_transceiver_sample = Instant::now();
            let mut last_unknown_source_log = Instant::now();
            let mut unknown_source_total = 0usize;

            let mut session_probe_failures = 0u32;
            while permanent_fault.is_none() && !stopping_loop.load(Ordering::Acquire) {
                match with_session(|session| Ok(session.needs_reconciliation())) {
                    Ok(true) => {
                        // Never clear the software flag or replay the uncertain frame.
                        // Withdraw activation, attempt TX off, and let procd create a
                        // fresh process that performs full reset/calibration/registration.
                        let mut backend = RuntimeOptics {
                            ready: ready_loop.load(Ordering::Acquire),
                            services: &mut services,
                            engine: &pon_engine_loop,
                            stopping: &stopping_loop,
                            #[cfg(feature = "npu")]
                            npu: Some(&npu_handle),
                        };
                        let response = optics.handle(OpticalCommand::Shutdown, &mut backend);
                        ready_loop.store(false, Ordering::Release);
                        if let Some(sender) = fault_sender.take() {
                            let _ = sender.send(format!(
                                "FSL management requires full reinitialization; shutdown: {response}"
                            ));
                        }
                        break;
                    }
                    Ok(false) => session_probe_failures = 0,
                    Err(error) if ready_loop.load(Ordering::Acquire) => {
                        // A session that was published and now cannot be probed
                        // (poisoned mutex or withdrawn slot) has no in-process
                        // recovery: the poll loop below would spin on errors.
                        session_probe_failures += 1;
                        if session_probe_failures >= 3 {
                            eprintln!(
                                "[-] FSL session probe failed {session_probe_failures} times: {error}"
                            );
                            ready_loop.store(false, Ordering::Release);
                            if let Some(sender) = fault_sender.take() {
                                let _ = sender.send(format!(
                                    "FSL session unreachable after being ready: {error}"
                                ));
                            }
                            break;
                        }
                    }
                    Err(_) => session_probe_failures = 0,
                }
                {
                    let mut backend = RuntimeOptics {
                        ready: ready_loop.load(Ordering::Acquire),
                        services: &mut services,
                        engine: &pon_engine_loop,
                        stopping: &stopping_loop,
                        #[cfg(feature = "npu")]
                        npu: Some(&npu_handle),
                    };
                    for request in optical_receiver.try_iter().take(2) {
                        let response = optics.dispatch(&request, &mut backend, &stopping_loop);
                        let _ = request.reply.send(response);
                    }
                    if backend.ready() && !stopping_loop.load(Ordering::Acquire) && mac_poll.due() {
                        if let Err(error) = optics.reconcile(&mut backend) {
                            eprintln!("[-] FSL GPON controls: {error}");
                        }
                    }
                }
                if stopping_loop.load(Ordering::Acquire) {
                    break;
                }
                if ready_loop.load(Ordering::Acquire) && Instant::now() >= next_transceiver_sample {
                    super::transceiver::refresh();
                    next_transceiver_sample = Instant::now() + Duration::from_secs(5);
                }
                if !ready_loop.load(Ordering::Acquire) || optics.state != OpticalState::Enabled
                    || crate::controller::pon_mac::snapshot().tx_enabled != Some(true) {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }

                let run_timeouts = last_timeout_check.elapsed() >= Duration::from_secs(1);
                let poll_result = with_session(|session| {
                    let report = session.poll_once(16, 16, &mut |delay| {
                        thread::sleep(delay);
                        Ok(())
                    })?;
                    observation_counts.0 += report.interrupts.len();
                    observation_counts.1 += report.ploams.len();
                    observation_counts.2 += report.flagged_ploams;
                    observation_counts.3 += report.omci_frames.len();
                    if observation_start.elapsed() >= Duration::from_secs(30) {
                        if receive_snapshots_remaining > 0 {
                            receive_snapshots_remaining -= 1;
                            if let Err(error) = log_receive_path_snapshot(session) {
                                eprintln!("[-] FSL RX snapshot failed: {error}");
                            }
                        }
                        if observation_first || observation_counts != (0, 0, 0, 0) {
                            println!(
                                "[*] FSL poll 30s: interrupts={} ploam={} flagged={} omci={}",
                                observation_counts.0,
                                observation_counts.1,
                                observation_counts.2,
                                observation_counts.3
                            );
                        }
                        observation_first = false;
                        observation_start = Instant::now();
                        observation_counts = (0, 0, 0, 0);
                    }

                    for frame in &report.omci_frames {
                        if let Ok(parsed) = BaselineFrame::from_bytes(&frame.payload) {
                            // Queue keys must match a live ONU: unknown sources
                            // would otherwise grow unbounded queues and mask a
                            // wrong bits21:14 interpretation behind silent drops.
                            if omci_source_is_live(session, frame.source) {
                                if let Ok(mut queues) = runtime_state().omci_rx.lock() {
                                    queue_omci_response(queues.entry(frame.source).or_default(), parsed);
                                }
                            } else {
                                unknown_source_total += 1;
                            }
                        }
                    }
                    if unknown_source_total > 0
                        && last_unknown_source_log.elapsed() >= Duration::from_secs(1)
                    {
                        // Report the accumulated total so the 1s rate limit
                        // cannot undercount sustained unknown-source traffic.
                        last_unknown_source_log = Instant::now();
                        println!(
                            "[*] FSL dropped {unknown_source_total} OMCI frame(s) from unknown/inactive ONU sources"
                        );
                        unknown_source_total = 0;
                    }

                    let mut deactivated = Vec::new();
                    match session.evaluate_alarm_interrupts(&report.interrupts) {
                        Ok(offline) => {
                            for (onu_id, reason) in offline {
                                println!(
                                    "[*] FSL offline alarm ({reason}) for ONU {onu_id} -> deactivating"
                                );
                                match deactivate_session_onu(session, onu_id) {
                                    Ok(true) => deactivated.push(onu_id),
                                    Ok(false) => {}
                                    Err(err) => eprintln!(
                                        "[-] FSL deactivate failed for ONU {onu_id} ({reason}): {err}"
                                    ),
                                }
                            }
                        }
                        Err(err) => eprintln!("[-] FSL alarm evaluation failed: {err}"),
                    }

                    let mut newly_active = Vec::new();
                    for record in &report.ploams {
                        if record.message_id() == SERIAL_NUMBER_ONU_MSG_ID {
                            let Some(rate) = OnuUpstreamRate::from_metadata(record.rate_metadata())
                            else {
                                eprintln!(
                                    "[-] Ignoring SN with unsupported upstream rate metadata {}",
                                    record.rate_metadata()
                                );
                                continue;
                            };
                            match record.onu_id() {
                                None => {
                                    if let Some(parsed) = super::olt::parse_upstream_ploam(record) {
                                        let mut sn = [0u8; 8];
                                        sn.copy_from_slice(&parsed.payload[..8]);
                                        if let Some(existing) = session.find_onu_by_sn(&sn) {
                                            if existing.state == FslOnuState::Active {
                                                println!(
                                                    "[*] O3 SN rediscovery for active ONU {} -> deactivating stale registration",
                                                    existing.onu_id
                                                );
                                                match deactivate_session_onu(session, existing.onu_id)
                                                {
                                                    Ok(true) => deactivated.push(existing.onu_id),
                                                    Ok(false) => {}
                                                    Err(err) => eprintln!(
                                                        "[-] Stale ONU {} deactivation failed: {err}",
                                                        existing.onu_id
                                                    ),
                                                }
                                            }
                                        }
                                        match session.process_o3_sn_discovery(
                                            sn,
                                            rate,
                                            &mut |delay| {
                                                thread::sleep(delay);
                                                Ok(())
                                            },
                                        ) {
                                            Ok(onu) => eprintln!(
                                                "[+] O3 SN accepted -> ONU {} ranging (rate={rate:?})",
                                                onu.onu_id
                                            ),
                                            Err(err) => {
                                                eprintln!("[-] O3 SN discovery failed: {err}")
                                            }
                                        }
                                    }
                                }
                                Some(onu_id)
                                    if session
                                        .onu(onu_id)
                                        .is_some_and(|onu| onu.state == FslOnuState::Ranging) =>
                                {
                                    let mut serial_number = [0u8; 8];
                                    serial_number.copy_from_slice(&record.frame[2..10]);
                                    match session.process_o4_o5_activation(
                                        onu_id,
                                        &serial_number,
                                        record.ranging_time_raw(),
                                        &mut |delay| {
                                            thread::sleep(delay);
                                            Ok(())
                                        },
                                    ) {
                                        Ok(pending) => eprintln!(
                                            "[+] O4 ranging + Configure_Port_ID sent for ONU {} (eqd={}, state={:?})",
                                            pending.onu_id, pending.eqd, pending.state
                                        ),
                                        Err(err) => eprintln!(
                                            "[-] O4 activation failed for ONU {onu_id}: {err}"
                                        ),
                                    }
                                }
                                Some(_) => {}
                            }
                        } else if record.message_id() == PLOAM_MSG_DYING_GASP {
                            if let Some(onu_id) = record.onu_id() {
                                if session
                                    .onu(onu_id)
                                    .is_some_and(|onu| onu.state == FslOnuState::Active)
                                {
                                    println!(
                                        "[*] ONU {onu_id} sent Dying_Gasp PLOAM -> deactivating"
                                    );
                                    match deactivate_session_onu(session, onu_id) {
                                        Ok(true) => deactivated.push(onu_id),
                                        Ok(false) => {}
                                        Err(err) => eprintln!(
                                            "[-] Dying_Gasp deactivate failed for ONU {onu_id}: {err}"
                                        ),
                                    }
                                }
                            }
                        } else if record.message_id() == PLOAM_MSG_ACK {
                            match session.process_ploam_ack(record, &mut |delay| {
                                thread::sleep(delay);
                                Ok(())
                            }) {
                                Ok(Some(active)) => {
                                    eprintln!(
                                        "[+] O5 Configure_Port_ID ACK confirmed for ONU {} (rate={:?}, eqd={})",
                                        active.onu_id, active.rate, active.eqd
                                    );
                                    newly_active.push(active);
                                }
                                Ok(None) if record.frame[2] == PLOAM_MSG_ASSIGN_ALLOC_ID => {
                                    println!(
                                        "[*] FSL Assign_Alloc-ID ACK onu_id={:?} enable={} seq={}",
                                        record.onu_id(),
                                        record.frame[5],
                                        u16::from_be_bytes([record.frame[8], record.frame[9]])
                                    );
                                }
                                Ok(None) if record.frame[2] != PLOAM_MSG_BER_INTERVAL => {
                                    println!(
                                        "[*] Ignoring unmatched PLOAM ACK onu_id={:?} acked_msg={:#x} enable={} seq={}",
                                        record.onu_id(),
                                        record.frame[2],
                                        record.frame[5],
                                        u16::from_be_bytes([record.frame[8], record.frame[9]])
                                    );
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    eprintln!("[-] PLOAM ACK processing failed: {err}");
                                }
                            }
                        } else {
                            println!(
                                "[*] FSL upstream PLOAM msg_id={:#x} onu_id={:?}",
                                record.message_id(),
                                record.onu_id()
                            );
                        }
                    }

                    if run_timeouts {
                        last_timeout_check = Instant::now();
                        match session.check_timeouts(&mut |delay| {
                            thread::sleep(delay);
                            Ok(())
                        }) {
                            Ok(timeouts)
                                if timeouts.expired_acks > 0
                                    || !timeouts.timed_out_ranging.is_empty()
                                    || !timeouts.timed_out_port_id_ack.is_empty() =>
                            {
                                println!(
                                    "[*] FSL timeout sweep: expired_acks={} ranging_timeout={:?} port_id_ack_timeout={:?}",
                                    timeouts.expired_acks,
                                    timeouts.timed_out_ranging,
                                    timeouts.timed_out_port_id_ack
                                );
                            }
                            Ok(_) => {}
                            Err(err) => eprintln!("[-] FSL timeout sweep failed: {err}"),
                        }
                    }
                    // A later event in the same batch may have retired an earlier ACK.
                    newly_active.retain(|candidate| {
                        session.onu(candidate.onu_id).is_some_and(|current| {
                            current.state == FslOnuState::Active
                                && current.serial_number == candidate.serial_number
                        })
                    });
                    Ok((deactivated, newly_active))
                });

                match poll_result {
                    Ok((deactivated, newly_active)) => {
                        for id in deactivated {
                            services.stop(id);
                            if let Ok(mut engine) = pon_engine_loop.lock() {
                                engine.mark_offline(id);
                            }
                            if let Ok(mut queues) = runtime_state().omci_rx.lock() {
                                queues.remove(&id);
                            }
                            let _ = super::vport::set_port_up(id, false);
                        }
                        for onu in newly_active {
                            let mut vendor = [0u8; 4];
                            vendor.copy_from_slice(&onu.serial_number[..4]);
                            let mut sn_tail = [0u8; 4];
                            sn_tail.copy_from_slice(&onu.serial_number[4..8]);
                            let displaced = match pon_engine_loop.lock() {
                                Ok(mut engine) => match engine.confirm_platform_onu_with_rate(
                                    onu.onu_id,
                                    vendor,
                                    sn_tail,
                                    onu.eqd,
                                    Some(onu.rate.stock_pon_mode()),
                                    Some(onu.rate.uplink_gbps()),
                                ) {
                                    Ok(displaced) => displaced,
                                    Err(err) => {
                                        eprintln!("[-] FSL ONU confirmation failed: {err}");
                                        continue;
                                    }
                                },
                                Err(_) => {
                                    eprintln!("[-] PON engine lock poisoned");
                                    continue;
                                }
                            };
                            for id in displaced {
                                services.stop(id);
                            }
                            let vendor_str = String::from_utf8_lossy(&vendor).into_owned();
                            services.start(onu.onu_id, vendor_str);
                        }
                    }
                    Err(err) => {
                        eprintln!("[-] FSL poll error: {err}");
                        thread::sleep(Duration::from_millis(50));
                    }
                }
                services.refresh_auth();
                thread::sleep(Duration::from_millis(5));
            }

            // Drain any final Shutdown/Status request queued right as `stopping` flipped.
            {
                let mut backend = RuntimeOptics {
                    ready: ready_loop.load(Ordering::Acquire),
                    services: &mut services,
                    engine: &pon_engine_loop,
                    stopping: &stopping_loop,
                    #[cfg(feature = "npu")]
                    npu: Some(&npu_handle),
                };
                let mut handled_shutdown = false;
                for request in optical_receiver.try_iter() {
                    handled_shutdown |= matches!(request.command, OpticalCommand::Shutdown);
                    let response = optics.dispatch(&request, &mut backend, &stopping_loop);
                    let _ = request.reply.send(response);
                }
                // Bounded drain: a request queued right as `stopping` flipped may
                // still be in flight from the sender side; the main thread waits
                // up to 4s for its Shutdown confirmation, so stay well within it.
                let drain_deadline = Instant::now() + Duration::from_millis(500);
                while !handled_shutdown && Instant::now() < drain_deadline {
                    match optical_receiver.recv_timeout(Duration::from_millis(50)) {
                        Ok(request) => {
                            handled_shutdown |= matches!(request.command, OpticalCommand::Shutdown);
                            let response = optics.dispatch(&request, &mut backend, &stopping_loop);
                            let _ = request.reply.send(response);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
            if let Err(error) = super::vport::bring_all_ports_down() { eprintln!("[-] FSL shutdown VPORT cleanup: {error}"); }
            if let Ok(mut guard) = runtime_state().session.lock() {
                *guard = None;
            }
        })
        // The closure owns the fault sender; a spawn failure drops it, which
        // resolves `fault_receiver` below into a supervised restart.
        .inspect_err(|error| eprintln!("[-] FSL loop thread spawn failed: {error}"))
        .ok();

    let restart = tokio::select! {
        result = lifetime.wait(devices) => {
            eprintln!("[-] Controller supervision stopped: {result:?}");
            true
        }
        result = fault_receiver => {
            eprintln!("[-] FSL owner stopped: {result:?}; requesting supervised restart");
            true
        }
        result = crate::runtime::termination() => {
            if let Err(error) = result {
                eprintln!("[-] Signal handler failed: {error}");
            }
            false
        }
    };
    crate::controller::mqtt_bridge::shutdown();
    crate::controller::acs_bridge::shutdown();
    #[cfg(feature = "acs")]
    crate::acs::shutdown();
    #[cfg(feature = "npu")]
    npu.stop();
    crate::runtime::request_stop();
    let optical =
        crate::runtime::spawn_blocking(move || shutdown_client.request(OpticalCommand::Shutdown));
    let (optical, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(4), optical),
        crate::runtime::shutdown(),
    );
    match optical {
        Ok(Ok(Ok(_))) => (),
        result => eprintln!("[-] Optical shutdown not confirmed: {result:?}"),
    }
    // Defensive: the optical client usually set this before dispatching Shutdown;
    // keep the store so the fsl loop also stops when the client never got through.
    stopping.store(true, Ordering::Release);
    if let Some(handle) = fsl_thread {
        let _ = handle.join();
    }
    if restart {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod init_recovery_tests {
    use super::*;

    fn failing() -> io::Result<OpticalState> {
        Err(io::Error::new(io::ErrorKind::NotConnected, "no device"))
    }

    #[test]
    fn successful_or_stopped_initialization_never_requests_a_fault() {
        assert_eq!(init_fault_message(&Ok(OpticalState::Disabled), false), None);
        assert_eq!(init_fault_message(&Ok(OpticalState::Enabled), true), None);
        assert_eq!(init_fault_message(&failing(), true), None);
    }

    #[test]
    fn persistent_initialization_failure_requests_exactly_one_fault() {
        let message = match init_fault_message(&failing(), false) {
            Some(message) => message,
            None => panic!("expected a permanent-failure fault message"),
        };
        assert!(message.contains("failed permanently"));
        assert!(message.contains("no device"));
        // The shutdown-interrupted case must stay a clean stop, not a fault.
        assert_eq!(init_fault_message(&failing(), true), None);
    }

    #[test]
    fn retries_are_bounded_to_failed_and_running_states() {
        assert!(!init_should_retry(&Ok(OpticalState::Disabled), false));
        assert!(!init_should_retry(&failing(), true));
        assert!(init_should_retry(&failing(), false));
        // A rejected platform invariant (Unsupported) is deterministic; the
        // bounded retries cannot change its outcome.
        let unsupported = Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "GPON_ONUID_OFFSET_CFG read back 4",
        ));
        assert!(!init_should_retry(&unsupported, false));
    }

    #[test]
    fn read_tx_reports_the_register_bit_even_when_the_flag_disagrees() {
        use crate::platform::resources::PonMacReadback;
        // 0x102 bit4 is TX_DISABLE (inverted); bit0 of 0x202 is FEC.
        let disabled = PonMacReadback {
            raw: Some(0x10),
            tx_enabled: Some(false),
            fec_enabled: Some(true),
        };
        let enabled = PonMacReadback {
            raw: Some(0x00),
            tx_enabled: Some(true),
            fec_enabled: Some(false),
        };
        assert_eq!(readback_tx_enabled(disabled).unwrap(), false);
        assert_eq!(readback_tx_enabled(enabled).unwrap(), true);
        assert_eq!(
            readback_tx_enabled(PonMacReadback {
                raw: None,
                tx_enabled: None,
                fec_enabled: None
            })
            .unwrap_err()
            .to_string()
            .contains("lost the TX bit"),
            true
        );
    }
}
