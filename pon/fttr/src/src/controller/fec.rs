//! Serialized, identity-bound downstream FEC PM lifecycle and diagnostics.
use crate::pon_engine::omci::{error, OmciSession};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Command {
    onu_id: u8,
    serial: String,
    generation: u64,
    ani: u16,
    #[serde(default)]
    create_pm: bool,
}
pub(crate) struct Request {
    fec_generation: u64,
    command: Command,
    deadline: Instant,
    pub reply: std::sync::mpsc::Sender<io::Result<serde_json::Value>>,
}

/// Read live PM, optionally creating only a missing standard class 312 instance.
pub fn read(id: u8, create_pm: bool) -> io::Result<()> {
    // The daemon owns policy; report disabled support before ANI discovery errors.
    let status: fttr_api::StatusReply =
        serde_json::from_str(&crate::ipc::send_ipc_command("STATUS")?).map_err(io::Error::other)?;
    let onu = status
        .sub_gateways
        .iter()
        .find(|onu| onu.onu_id == id)
        .ok_or_else(|| io::Error::other("ONU not found"))?;
    let ani = onu
        .fec
        .as_ref()
        .and_then(|fec| fec.ani_entity_id)
        .ok_or_else(|| {
            io::Error::other("FEC disabled or awaiting current ANI-G discovery; check optical.fec_enabled and retry after telemetry poll")
        })?;
    let command = Command {
        onu_id: id,
        serial: onu.serial_number.clone(),
        generation: onu.business_status.generation,
        ani,
        create_pm,
    };
    let response = crate::ipc::send_ipc_command(&format!(
        "FEC_PM {}",
        serde_json::to_string(&command).map_err(io::Error::other)?
    ))?;
    let value: serde_json::Value = serde_json::from_str(&response).map_err(io::Error::other)?;
    if let Some(error) = value.get("error") {
        return Err(io::Error::other(error.to_string()));
    }
    println!("{response}");
    Ok(())
}

pub(crate) fn control(
    command: Command,
    states: &super::service::States,
) -> io::Result<serde_json::Value> {
    require_enabled()?;
    if !super::service::operation_confirmed(command.onu_id)
        || super::service::onu_serial(command.onu_id).as_deref() != Some(command.serial.as_str())
        || super::telemetry::snapshot(command.onu_id, true)
            .fec
            .ani_entity_id
            != Some(command.ani)
    {
        return Err(io::Error::other("ONU identity or current ANI-G mismatch"));
    }
    let (sender, wake) = states
        .lock()
        .unwrap()
        .get(&command.onu_id)
        .filter(|state| state.generation == command.generation)
        .and_then(|state| Some((state.mib.clone()?, state.wake.clone()?)))
        .ok_or_else(|| io::Error::other("ONU generation changed or monitor unavailable"))?;
    let (reply, receive) = std::sync::mpsc::channel();
    sender
        .try_send(super::inventory::WorkerRequest::Fec(Request {
            fec_generation: crate::optical::fec_generation(),
            command,
            deadline: Instant::now() + Duration::from_secs(60),
            reply,
        }))
        .map_err(|_| io::Error::other("ONU diagnostic queue busy or stopped"))?;
    wake.notify();
    receive
        .recv_timeout(Duration::from_secs(65))
        .map_err(io::Error::other)?
}

pub(crate) fn run(
    platform: crate::platform::context::PlatformContext,
    request: &Request,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<serde_json::Value> {
    require_enabled()?;
    let c = &request.command;
    if Instant::now() >= request.deadline
        || request.fec_generation != crate::optical::fec_generation()
        || super::service::onu_serial(c.onu_id).as_deref() != Some(c.serial.as_str())
    {
        return Err(io::Error::other(
            "FEC diagnostic expired or identity changed",
        ));
    }
    let mut session = OmciSession::new(platform.transport(c.onu_id)?);
    let id = c.onu_id;
    let serial = c.serial.clone();
    let fec_generation = request.fec_generation;
    session.guard_registration(move || {
        super::service::operation_confirmed(id)
            && super::service::onu_serial(id).as_deref() == Some(serial.as_str())
            && crate::optical::fec_enabled()
            && crate::optical::fec_generation() == fec_generation
    });
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    session.finish_before(
        request
            .deadline
            .min(Instant::now() + Duration::from_secs(10)),
    );
    let created_pm = ensure_pm(&mut session, c.ani, c.create_pm)?;
    let status = super::telemetry::fec::Poll::default().read(&mut session, c.ani);
    Ok(serde_json::json!({"created_pm": created_pm, "fec": status}))
}

fn require_enabled() -> io::Result<()> {
    if crate::optical::fec_enabled() {
        Ok(())
    } else {
        Err(io::Error::other(
            "FEC is disabled; enable optical.fec_enabled with optical transmission first",
        ))
    }
}

pub(super) fn ensure_pm(session: &mut OmciSession, ani: u16, create_pm: bool) -> io::Result<bool> {
    // Only explicit opt-in and a protocol unknown-instance result permit CREATE.
    // Never reset existing counters, modify thresholds or delete a foreign instance.
    match session.get_attributes(312, ani, 0x8000) {
        Ok(_) => Ok(false),
        Err(error) if create_pm && error::is_unknown_instance(&error) => {
            session.create_attributes(
                312,
                ani,
                &[(
                    2,
                    crate::pon_engine::omci::attributes::AttributeValue::number(0),
                )],
            )?;
            session.get_attributes(312, ani, 0x8000)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
    use std::sync::{Arc, Mutex};
    struct Transport {
        requests: Arc<Mutex<Vec<u8>>>,
        pending: Option<BaselineFrame>,
        result: u8,
        created: bool,
    }
    impl OmciTransport for Transport {
        fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
            self.requests.lock().unwrap().push(frame.message_type);
            if frame.message_type == 0x44 {
                assert_eq!(frame.class_id, 312);
                assert_eq!(&frame.payload[..2], &[0, 0]);
                self.created = true;
            }
            self.pending = Some(frame.clone());
            Ok(())
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            Ok(self.pending.take().map(|mut frame| {
                let get = frame.message_type == 0x49;
                frame.message_type = if get { 0x29 } else { 0x24 };
                frame.payload = [0; 32];
                frame.payload[0] = if get && !self.created { self.result } else { 0 };
                frame.payload[1..3].copy_from_slice(&0x8000u16.to_be_bytes());
                frame
            }))
        }
    }
    #[test]
    fn pm_create_requires_explicit_request_and_unknown_instance_not_other_errors() {
        for (result, create, expected_ok, expected_actions) in [
            (0, true, true, vec![0x49]),
            (5, false, false, vec![0x49]),
            (5, true, true, vec![0x49, 0x44, 0x49]),
            (4, true, false, vec![0x49]),
            (9, true, false, vec![0x49]),
        ] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mut session = OmciSession::new(Transport {
                requests: requests.clone(),
                pending: None,
                result,
                created: false,
            });
            assert_eq!(ensure_pm(&mut session, 0x8001, create).is_ok(), expected_ok);
            assert_eq!(*requests.lock().unwrap(), expected_actions);
        }
    }
}
