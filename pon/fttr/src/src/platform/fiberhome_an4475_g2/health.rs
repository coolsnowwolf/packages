//! Serialized OMCI liveness, diagnostic work, and per-ONU VPORT carrier gating
//! on the FSL transport (`fttr1..fttr128` 802.1ad ports over `fttr0`).
use super::{port_gate::PortGate, vport, RESOURCES};
use crate::{
    controller::{attribute_probe, fec, inventory, service, telemetry},
    platform::{
        context::PlatformContext,
        openwrt::{onu_lock::lock_onu, wake::Wake},
    },
    pon_engine::omci::OmciSession,
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc,
    },
    time::{Duration, Instant},
};

const PORT_STALE_TIMEOUT: Duration = Duration::from_secs(15);
const WATCH_LOSS_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn watch(
    platform: PlatformContext,
    id: u8,
    cancellation: Arc<AtomicBool>,
    requests: &Receiver<inventory::WorkerRequest>,
    wake: Arc<Wake>,
) -> io::Result<()> {
    let _owner = lock_onu(id)?;
    let serial = service::onu_serial(id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "ONU identity unavailable"))?;
    let _lease = platform.management_lease(id)?;
    let _channel = platform.channel(id, 0)?;
    // Begin closed and reopen only after a fresh OMCI liveness GET succeeds.
    let gate = PortGate::new(PORT_STALE_TIMEOUT, cancellation.clone(), move |up| {
        if up {
            crate::omci_auth::admit(id, || vport::set_port_up(id, true))
        } else {
            vport::set_port_up(id, false)
        }
    })?;
    let management = crate::provisioning::health_watch::subscribe(&serial, wake.clone());
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || service::operation_confirmed(id));
    session.cancel_when(cancellation.clone());
    let mut collector = telemetry::Collector::new(platform, id, cancellation.clone());
    let mut next_get = Instant::now();
    let mut next_telemetry = None;
    let mut last_response = Instant::now();
    loop {
        wake.drain();
        if cancellation.load(Ordering::Acquire) {
            return Ok(());
        }
        if !service::operation_confirmed(id)
            || service::onu_serial(id).as_deref() != Some(serial.as_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "ONU registration changed",
            ));
        }
        if management.changed() {
            return Err(io::Error::other("OMCI management configuration changed"));
        }
        if let Ok(request) = requests.try_recv() {
            // An explicit MIB request starts a new ONU snapshot; discard any
            // automatic upload before processing any diagnostic.
            collector.interrupt_upload();
            // The synchronous diagnostic may exceed the evidence deadline.
            // Keep the port closed until a subsequent liveness GET succeeds.
            gate.close()?;
            match request {
                inventory::WorkerRequest::Fec(request) => {
                    let result = fec::run(platform, &request, Some(cancellation.clone()));
                    let _ = request.reply.send(result);
                }
                inventory::WorkerRequest::Attribute(request) => {
                    let result =
                        attribute_probe::run(platform, &request, Some(cancellation.clone()));
                    let _ = request.reply.send(result);
                }
                inventory::WorkerRequest::Mib(request) => {
                    let result = if Instant::now() >= request.deadline {
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "MIB request expired before monitor became ready",
                        ))
                    } else {
                        inventory::read_owned(
                            platform,
                            id,
                            request.tables,
                            Some(cancellation.clone()),
                            request.deadline,
                            request.progress,
                            request.registration,
                        )
                    };
                    let _ = request.reply.send(result);
                }
            }
            // Diagnostics may take a long time, but completing one is not an
            // OMCI liveness response. Recheck before accepting the ONU as live.
            next_get = Instant::now();
        }
        if Instant::now() >= next_get {
            match session.with_timeout(Duration::from_secs(4), |session| {
                session.get(256, 0, 0x8000)
            }) {
                Ok(_) => {
                    last_response = Instant::now();
                    if gate.responsive(last_response)? {
                        println!(
                            "VERIFIED_LINK_ENABLED interface={} OMCI_GET=true watch=true; management/Wi-Fi requires separate verification",
                            RESOURCES.interface(id).unwrap_or("unmapped")
                        );
                    }
                    if next_telemetry.is_none() {
                        next_telemetry = Some(last_response);
                    }
                }
                Err(_) if cancellation.load(Ordering::Acquire) => return Ok(()),
                Err(error) if last_response.elapsed() < WATCH_LOSS_TIMEOUT => {
                    eprintln!("ONU {id} OMCI liveness sample unavailable: {error}");
                }
                Err(error) => return Err(error),
            }
            next_get = Instant::now() + Duration::from_secs(5);
        }
        if gate.is_open() && next_telemetry.is_some_and(|deadline| Instant::now() >= deadline) {
            if last_response.elapsed() < Duration::from_secs(1) {
                // All telemetry transactions share this deadline, including MIB
                // discovery. Never let an automatic poll outlive fresh evidence.
                collector.refresh_bounded(last_response + Duration::from_secs(8));
                let completed = Instant::now();
                next_get = completed;
                next_telemetry = Some(
                    completed
                        + Duration::from_secs(if collector.upload_pending() { 5 } else { 30 }),
                );
            } else {
                // Refresh liveness before spending its remaining budget.
                next_get = Instant::now();
            }
        }
        wake.wait(&[], next_get.min(Instant::now() + Duration::from_secs(1)))?;
    }
}
