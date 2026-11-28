//! HWTC standard OMCI bridge profile, initially verified on K155-11.
use crate::controller::business::verify;
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::OmciSession;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub fn configure(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    configure_profile(platform, id, cancellation, super::Profile::Standard)
}

/// First-stage V152 provisioning: a transparent, untagged Ethernet path to
/// the non-OMCI domain. Internal WAN allocation still requires device evidence.
pub(super) fn configure_profile(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
    profile: super::Profile,
) -> io::Result<()> {
    platform.resources().onu(id)?;
    let _lock = lock_onu(id)?;
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || crate::controller::service::operation_confirmed(id));
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    let mut lease = platform.management_lease(id)?;
    let _channel = platform.channel(id, 0)?;
    verify(&mut session, 256, 0, 0x8000, b"HWTC")?;
    if profile == super::Profile::V152 {
        super::v152::service::verify_identity(&mut session)?;
    }
    if profile == super::Profile::K155 {
        super::k155::service::verify_identity(&mut session)?;
    }
    // Dispatch is vendor-wide by request. Confirm the live ONU-G vendor before
    // any profile writes; per-model topology still requires validation.
    // The live MIB exposes these ONU-created endpoints and priority queues.
    for (class, instance) in [
        (262, 0x8000),
        (277, 0x8000),
        (11, 0x101),
        (11, 0x102),
        (329, 0x601),
    ] {
        session.get(class, instance, 0x8000)?;
    }
    crate::omci_auth::admit(id, || Ok(()))?;
    let port = platform.resources().onu(id)?.gem_id;
    super::bridge::configure(&mut session, port, platform.resources().onu(id)?.alloc_id)?;
    if profile == super::Profile::V152 {
        super::v152::service::after_bridge(&mut session)?;
    }
    verify(&mut session, 256, 0, 0x8000, b"HWTC")?;
    println!("HWTC_SERVICE_READBACK_OK gem={port:04x}; data evidence still required");
    crate::controller::management::bootstrap(&mut session, id, 0x601);
    lease.disarm()?;
    println!("HWTC_LEASE_PERSISTED id={id}");
    Ok(())
}
