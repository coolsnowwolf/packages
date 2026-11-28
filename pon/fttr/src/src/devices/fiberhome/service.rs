//! Standard discovered bridge plus bounded FiberHome grant policy.
//! HG6371F-C and HG6371F expose different UNI inventories; never use SCTY's
//! fixed-entity cleanup. Generic discovery handles bootstrap and slot migration.
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub(super) fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    platform.resources().onu(id)?;
    crate::devices::generic::service::configure_with_grant_floors(
        platform,
        id,
        cancellation,
        Some((60, 1300)),
    )?;
    println!("FIBERHOME_HG6371_BRIDGE_READY id={id} business=1300 management=60 GEM_RX=unverified");
    Ok(())
}
