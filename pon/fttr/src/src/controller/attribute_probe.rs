//! Identity-bound diagnostics serialized by the per-ONU lock.
use crate::pon_engine::omci::{
    attributes::AttributeValue,
    schema::{AttributeKind, MeRegistry},
    session::SingleShotError,
    tables::VlanRule,
    wire::{hex, unhex},
    OmciSession,
};
use std::{
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

// POLLHUP detects a closed peer without treating a write-half-close as a
// disconnect: existing IPC clients may half-close after sending a request.
fn peer_connected(stream: &UnixStream) -> bool {
    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut fd, 1, 0) };
    result >= 0 && fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Command {
    pub onu_id: u8,
    pub serial: String,
    pub generation: u64,
    pub class: u16,
    pub instance: u16,
    pub mask: u16,
    #[serde(default)]
    pub table: bool,
    #[serde(default)]
    pub expected: Option<Vec<u8>>,
    #[serde(default)]
    pub replacement: Option<Vec<u8>>,
}
pub(crate) struct Request {
    pub command: Command,
    pub deadline: Instant,
    pub reply: std::sync::mpsc::Sender<io::Result<serde_json::Value>>,
}
impl Command {
    fn snapshot_width(&self) -> io::Result<Option<usize>> {
        if self.expected.is_some() || self.replacement.is_some() {
            return Err(io::Error::other("snapshot is read-only"));
        }
        if self.table {
            if self.class != 171 || self.mask != 0x0400 {
                return Err(io::Error::other(
                    "only the EVTO operation table is supported",
                ));
            }
            Ok(None)
        } else {
            self.width().map(Some)
        }
    }
    fn width(&self) -> io::Result<usize> {
        let attr = MeRegistry::get(self.class)
            .filter(|def| def.layout_supported)
            .and_then(|def| def.attributes.iter().find(|attr| attr.mask == self.mask))
            .filter(|attr| {
                attr.read && attr.kind != AttributeKind::Table && attr.size > 0 && attr.size <= 25
            })
            .ok_or_else(|| io::Error::other("requires one readable, fixed-width attribute"))?;
        let width = usize::from(attr.size);
        match (&self.expected, &self.replacement) {
            (None, None) => {}
            (Some(before), Some(after))
                if attr.write && before.len() == width && after.len() == width => {}
            _ => {
                return Err(io::Error::other(
                    "SET requires writable attribute and exact-width expected/replacement bytes",
                ))
            }
        }
        Ok(width)
    }
    fn current(&self, states: &super::service::States) -> bool {
        identity_current(states, self.onu_id, &self.serial, self.generation)
    }
}
pub(crate) fn control(
    command: Command,
    states: &super::service::States,
) -> io::Result<serde_json::Value> {
    if command.expected.is_some() || command.replacement.is_some() || command.table {
        return Err(io::Error::other(
            "writes and table reads are not available on the queued probe",
        ));
    }
    command.width()?;
    if !command.current(states) {
        return Err(io::Error::other(
            "ONU identity/registration generation mismatch",
        ));
    }
    let (sender, wake) = states
        .lock()
        .unwrap()
        .get(&command.onu_id)
        .and_then(|s| Some((s.mib.clone()?, s.wake.clone()?)))
        .ok_or_else(|| io::Error::other("ONU has no active monitor"))?;
    let (reply, receive) = std::sync::mpsc::channel();
    sender
        .try_send(super::inventory::WorkerRequest::Attribute(Request {
            command,
            deadline: Instant::now() + Duration::from_secs(600),
            reply,
        }))
        .map_err(|_| io::Error::other("ONU diagnostic queue busy or stopped"))?;
    wake.notify();
    receive
        .recv_timeout(Duration::from_secs(605))
        .map_err(io::Error::other)?
}

pub(crate) fn identity_current(
    states: &super::service::States,
    onu_id: u8,
    serial: &str,
    generation: u64,
) -> bool {
    super::service::operation_confirmed(onu_id)
        && super::service::onu_serial(onu_id).as_deref() == Some(serial)
        && states
            .lock()
            .unwrap()
            .get(&onu_id)
            .is_some_and(|s| s.generation == generation)
}

fn open_direct_session(
    platform: crate::platform::context::PlatformContext,
    onu_id: u8,
    serial: &str,
    generation: u64,
    states: &super::service::States,
    stream: &UnixStream,
) -> io::Result<(
    std::fs::File,
    Box<dyn crate::platform::context::ManagementLease>,
    Box<dyn crate::platform::context::ChannelGuard>,
    OmciSession,
)> {
    if !peer_connected(stream) {
        return Err(io::Error::other("diagnostic client disconnected"));
    }
    if !identity_current(states, onu_id, serial, generation) {
        return Err(io::Error::other(
            "ONU identity/registration generation mismatch",
        ));
    }
    let owner = crate::platform::openwrt::onu_lock::lock_onu(onu_id)?;
    if !peer_connected(stream) {
        return Err(io::Error::other("diagnostic client disconnected"));
    }
    if !identity_current(states, onu_id, serial, generation) {
        return Err(io::Error::other(
            "ONU identity/registration generation mismatch",
        ));
    }
    let lease = platform.management_lease(onu_id)?;
    let channel = platform.channel(onu_id, 0)?;
    let mut session = OmciSession::new(platform.transport(onu_id)?);
    let serial_owned = serial.to_string();
    let states = states.clone();
    let peer = stream.try_clone()?;
    session.guard_registration(move || {
        peer_connected(&peer) && identity_current(&states, onu_id, &serial_owned, generation)
    });
    session.finish_before(Instant::now() + Duration::from_secs(30));
    Ok((owner, lease, channel, session))
}

/// Read only, without queuing: a busy worker must be retried explicitly by the caller.
pub(crate) fn snapshot(
    platform: crate::platform::context::PlatformContext,
    command: Command,
    states: &super::service::States,
    stream: &UnixStream,
) -> io::Result<serde_json::Value> {
    let width = command.snapshot_width()?;
    let (_owner, _lease, _channel, mut session) = open_direct_session(
        platform,
        command.onu_id,
        &command.serial,
        command.generation,
        states,
        stream,
    )?;
    let data = if let Some(width) = width {
        session
            .get(command.class, command.instance, command.mask)?
            .payload[3..3 + width]
            .to_vec()
    } else {
        let table = session.get_table(command.class, command.instance, command.mask)?;
        if table.len() % 16 != 0 {
            return Err(io::Error::other("EVTO table has a partial row"));
        }
        table
    };
    Ok(serde_json::json!({"data":data,"table":command.table}))
}

/// Strictly single-shot fixed-width attribute SET with compare-before-SET and
/// immediate readback, never queued and never retried.
pub(crate) fn set_once(
    platform: crate::platform::context::PlatformContext,
    command: Command,
    states: &super::service::States,
    stream: &UnixStream,
) -> io::Result<serde_json::Value> {
    let width = command.width()?;
    let Some(after) = command.replacement.as_ref() else {
        return Err(io::Error::other("OMCI_SET_ONCE requires replacement bytes"));
    };
    let Some(expected) = command.expected.as_ref() else {
        return Err(io::Error::other("OMCI_SET_ONCE requires expected bytes"));
    };
    let (_owner, _lease, _channel, mut session) = open_direct_session(
        platform,
        command.onu_id,
        &command.serial,
        command.generation,
        states,
        stream,
    )?;
    let before = session
        .get(command.class, command.instance, command.mask)?
        .payload[3..3 + width]
        .to_vec();
    if &before != expected {
        return Err(io::Error::other(format!(
            "compare-before-SET mismatch; actual_hex={}",
            hex(&before)
        )));
    }
    match session.set_once(command.class, command.instance, command.mask, after) {
        Ok(()) => {
            let after_read = session
                .get(command.class, command.instance, command.mask)
                .map(|frame| frame.payload[3..3 + width].to_vec());
            Ok(serde_json::json!({
                "outcome": "accepted",
                "before_hex": hex(&before),
                "after_hex": after_read.as_ref().ok().map(|b| hex(b)),
                "readback_error": after_read.as_ref().err().map(ToString::to_string),
            }))
        }
        Err(SingleShotError::NotSent(error)) => Ok(serde_json::json!({
            "outcome": "not_sent",
            "error": error.to_string(),
            "before_hex": hex(&before),
        })),
        Err(SingleShotError::Rejected(error)) => Ok(serde_json::json!({
            "outcome": "rejected",
            "error": error.to_string(),
            "before_hex": hex(&before),
        })),
        Err(SingleShotError::UncertainAfterSend(error)) => Ok(serde_json::json!({
            "outcome": "uncertain_after_send",
            "error": error.to_string(),
            "before_hex": hex(&before),
        })),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvtoRuleMode {
    Write,
    Delete,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvtoRuleCommand {
    pub onu_id: u8,
    pub serial: String,
    pub generation: u64,
    pub instance: u16,
    pub mode: EvtoRuleMode,
    pub row_hex: String,
    pub expected_before_hex: Vec<String>,
}

impl EvtoRuleCommand {
    fn validate(&self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        let row = unhex(&self.row_hex)?;
        if row.len() != 16 {
            return Err(io::Error::other("EVTO rule must be exactly 16 bytes"));
        }
        if self.mode == EvtoRuleMode::Delete && !row[8..].iter().all(|b| *b == 0xff) {
            return Err(io::Error::other(
                "EVTO delete rule must have 0xffffffffffffffff in bytes 8..16",
            ));
        }
        let mut expected_before = Vec::with_capacity(self.expected_before_hex.len() * 16);
        for row_hex in &self.expected_before_hex {
            let decoded = unhex(row_hex)?;
            if decoded.len() != 16 {
                return Err(io::Error::other(
                    "expected_before_hex entries must each be 16 bytes",
                ));
            }
            expected_before.extend_from_slice(&decoded);
        }
        Ok((row, expected_before))
    }
}

fn split_evto_rows_hex(bytes: &[u8]) -> io::Result<Vec<String>> {
    if bytes.len() % 16 != 0 {
        return Err(io::Error::other("EVTO table has a partial row"));
    }
    Ok(bytes.chunks_exact(16).map(hex).collect())
}

fn read_evto_context(session: &mut OmciSession, instance: u16) -> serde_json::Value {
    let assoc_type = session
        .get(171, instance, 0x8000)
        .ok()
        .map(|f| f.payload[3]);
    let input_tpid = session
        .get(171, instance, 0x2000)
        .ok()
        .map(|f| hex(&f.payload[3..5]));
    let output_tpid = session
        .get(171, instance, 0x1000)
        .ok()
        .map(|f| hex(&f.payload[3..5]));
    let downstream_mode = session
        .get(171, instance, 0x0800)
        .ok()
        .map(|f| f.payload[3]);
    let associated_me = session
        .get(171, instance, 0x0200)
        .ok()
        .map(|f| hex(&f.payload[3..5]));
    serde_json::json!({
        "association_type": assoc_type,
        "input_tpid_hex": input_tpid,
        "output_tpid_hex": output_tpid,
        "downstream_mode": downstream_mode,
        "associated_me_hex": associated_me,
    })
}

/// Strictly single-shot EVTO table rule write or keyed deletion with baseline
/// verification and immediate readback. Never retries an uncertain SET.
pub(crate) fn evto_rule(
    platform: crate::platform::context::PlatformContext,
    command: EvtoRuleCommand,
    states: &super::service::States,
    stream: &UnixStream,
) -> io::Result<serde_json::Value> {
    let (row, expected_before) = command.validate()?;
    let (_owner, _lease, _channel, mut session) = open_direct_session(
        platform,
        command.onu_id,
        &command.serial,
        command.generation,
        states,
        stream,
    )?;
    let context = read_evto_context(&mut session, command.instance);
    let before = session.get_table(171, command.instance, 0x0400)?;
    let before_rows = split_evto_rows_hex(&before)?;
    if before != expected_before {
        return Err(io::Error::other(format!(
            "compare-before-SET EVTO table mismatch; actual_rows={before_rows:?}"
        )));
    }
    match session.set_attributes_once(171, command.instance, &[(6, AttributeValue::octets(&row))]) {
        Ok(()) => {
            let after_result = session.get_table(171, command.instance, 0x0400);
            match after_result {
                Ok(after) => {
                    let after_rows = split_evto_rows_hex(&after)?;
                    let decoded_after_rows: Vec<_> = after
                        .chunks_exact(16)
                        .map(VlanRule::decode)
                        .collect::<io::Result<Vec<_>>>()?;
                    Ok(serde_json::json!({
                        "outcome": "accepted",
                        "context": context,
                        "before_rows": before_rows,
                        "after_rows": after_rows,
                        "decoded_after_rows": decoded_after_rows,
                    }))
                }
                Err(error) => Ok(serde_json::json!({
                    "outcome": "accepted",
                    "context": context,
                    "before_rows": before_rows,
                    "after_table_error": error.to_string(),
                })),
            }
        }
        Err(SingleShotError::NotSent(error)) => Ok(serde_json::json!({
            "outcome": "not_sent",
            "error": error.to_string(),
            "context": context,
            "before_rows": before_rows,
        })),
        Err(SingleShotError::Rejected(error)) => {
            let after_rows = session
                .get_table(171, command.instance, 0x0400)
                .and_then(|bytes| split_evto_rows_hex(&bytes));
            Ok(serde_json::json!({
                "outcome": "rejected",
                "error": error.to_string(),
                "context": context,
                "before_rows": before_rows,
                "after_rows": after_rows.as_ref().ok(),
                "after_table_error": after_rows.as_ref().err().map(ToString::to_string),
            }))
        }
        Err(SingleShotError::UncertainAfterSend(error)) => Ok(serde_json::json!({
            "outcome": "uncertain_after_send",
            "error": error.to_string(),
            "context": context,
            "before_rows": before_rows,
        })),
    }
}
pub(crate) fn run(
    platform: crate::platform::context::PlatformContext,
    request: &Request,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<serde_json::Value> {
    let command = &request.command;
    let width = command.width()?;
    // Called only by this generation's worker under its ONU/OMCC/channel leases.
    if Instant::now() >= request.deadline
        || super::service::onu_serial(command.onu_id).as_deref() != Some(command.serial.as_str())
    {
        return Err(io::Error::other(
            "diagnostic expired or ONU identity changed",
        ));
    }
    let mut session = OmciSession::new(platform.transport(command.onu_id)?);
    let id = command.onu_id;
    let serial = command.serial.clone();
    session.guard_registration(move || {
        super::service::operation_confirmed(id)
            && super::service::onu_serial(id).as_deref() == Some(serial.as_str())
    });
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    session.finish_before(
        request
            .deadline
            .min(Instant::now() + Duration::from_secs(30)),
    );
    let before = session
        .get(command.class, command.instance, command.mask)?
        .payload[3..3 + width]
        .to_vec();
    if let Some(after) = &command.replacement {
        if command.expected.as_ref() != Some(&before) {
            return Err(io::Error::other(
                "compare-before-SET mismatch; no write performed",
            ));
        }
        session.set(command.class, command.instance, command.mask, after)?;
        let readback = session.get(command.class, command.instance, command.mask);
        if !readback
            .as_ref()
            .is_ok_and(|frame| &frame.payload[3..3 + width] == after.as_slice())
        {
            let restore = session
                .set(command.class, command.instance, command.mask, &before)
                .and_then(|()| session.get(command.class, command.instance, command.mask))
                .and_then(|frame| {
                    if frame.payload[3..3 + width] == before {
                        Ok(())
                    } else {
                        Err(io::Error::other("restore readback mismatch"))
                    }
                });
            return Err(io::Error::other(format!(
                "SET readback failed; restore={restore:?}"
            )));
        }
    }
    Ok(serde_json::json!({"before":before,"after":command.replacement,"readback_ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> Command {
        serde_json::from_value(serde_json::json!({"onu_id":1,"serial":"<ONU_A_SN>",
            "generation":2,"class":268,"instance":257,"mask":4096}))
        .unwrap()
    }
    #[test]
    fn probe_requires_one_known_attribute_and_exact_compare_before_set() {
        let mut c = command();
        assert_eq!(c.width().unwrap(), 2);
        c.replacement = Some(vec![0x80, 0]);
        assert!(c.width().is_err());
        c.expected = Some(vec![0x80, 1]);
        assert_eq!(c.width().unwrap(), 2);
        c.replacement = Some(vec![0]);
        assert!(c.width().is_err());
        c.replacement = None;
        c.expected = None;
        c.mask = 0x3000;
        assert!(c.width().is_err());
    }
    #[test]
    fn probe_rejects_writes_to_read_only_attributes() {
        let mut c = command();
        c.class = 277;
        c.mask = 0x4000; // MaximumQueueSize: read only.
        assert_eq!(c.width().unwrap(), 2);
        c.expected = Some(vec![0, 0]);
        c.replacement = Some(vec![0, 1]);
        assert!(c.width().is_err());
    }

    #[test]
    fn probe_rejects_table_attributes_even_when_row_width_fits() {
        let mut c = command();
        c.class = 171;
        c.mask = 0x0400;
        assert!(c.width().is_err());
        c.expected = Some(vec![0; 16]);
        c.replacement = Some(vec![0; 16]);
        assert!(c.width().is_err());
    }

    #[test]
    fn snapshot_accepts_only_read_only_fixed_attributes_or_evto_table() {
        let mut c = command();
        assert_eq!(c.snapshot_width().unwrap(), Some(2));
        c.expected = Some(vec![0, 0]);
        c.replacement = Some(vec![0, 1]);
        assert!(c.snapshot_width().is_err());
        c.expected = None;
        c.replacement = None;
        c.class = 171;
        c.mask = 0x0400;
        assert!(c.snapshot_width().is_err());
        c.table = true;
        assert_eq!(c.snapshot_width().unwrap(), None);
        c.mask = 0x0800;
        assert!(c.snapshot_width().is_err());
        c.mask = 0x0400;
        c.class = 268;
        assert!(c.snapshot_width().is_err());
    }

    #[test]
    fn queued_probe_rejects_writes_before_identity_or_queue_access() {
        let mut c = command();
        c.expected = Some(vec![0, 0]);
        c.replacement = Some(vec![0, 1]);
        let states = super::super::service::States::default();
        assert!(control(c, &states)
            .unwrap_err()
            .to_string()
            .contains("writes"));
    }

    #[test]
    fn peer_disconnect_check_allows_write_half_close() {
        let (server, client) = UnixStream::pair().unwrap();
        assert!(peer_connected(&server));
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(peer_connected(&server));
        drop(client);
        assert!(!peer_connected(&server));
    }

    #[test]
    fn evto_rule_command_validates_lengths_and_delete_sentinel() {
        let mut cmd: EvtoRuleCommand = serde_json::from_value(serde_json::json!({
            "onu_id": 0,
            "serial": "<ONU_A_SN>",
            "generation": 1,
            "instance": 0x0201,
            "mode": "write",
            "row_hex": "f8000000f8000000000f0000000f0000",
            "expected_before_hex": []
        }))
        .unwrap();
        let (row, expected) = cmd.validate().unwrap();
        assert_eq!(row.len(), 16);
        assert!(expected.is_empty());

        cmd.row_hex = "f800".into();
        assert!(cmd.validate().is_err());

        cmd.mode = EvtoRuleMode::Delete;
        cmd.row_hex = "f8000000f8000000000f0000000f0000".into();
        assert!(cmd.validate().is_err());

        cmd.row_hex = "f8000000f8000000ffffffffffffffff".into();
        cmd.expected_before_hex = vec!["f8000000f8000000000f0000000f0000".into()];
        let (row, expected) = cmd.validate().unwrap();
        assert_eq!(row.len(), 16);
        assert_eq!(expected.len(), 16);
    }
}
