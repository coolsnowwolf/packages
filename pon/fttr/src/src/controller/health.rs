//! Platform-selected evidence monitor. OMCI liveness is not GEM/CWMP readiness.
use crate::platform::{context::PlatformContext, openwrt::wake::Wake};
use std::{
    io,
    sync::mpsc::Receiver,
    sync::{atomic::AtomicBool, Arc},
};

pub fn diagnostic() -> io::Result<()> {
    let platform = PlatformContext::detect().map_err(io::Error::other)?;
    platform.backend().health_diagnostic(platform)
}

pub(crate) fn watch(
    platform: PlatformContext,
    id: u8,
    cancellation: Arc<AtomicBool>,
    vendor: [u8; 4],
    requests: &Receiver<super::inventory::WorkerRequest>,
    wake: Arc<Wake>,
) -> io::Result<()> {
    platform.resources().onu(id)?;
    platform
        .backend()
        .watch(platform, id, cancellation, vendor, requests, wake)
}
