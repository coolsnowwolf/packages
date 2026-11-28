use crate::pon_engine::omci::OmciSession;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    super::super::service::configure_profile(
        platform,
        id,
        cancellation,
        super::super::Profile::K155,
    )
}

pub(in crate::devices::hwtc) fn verify_identity(session: &mut OmciSession) -> io::Result<()> {
    let equipment = session.get(257, 0, 0x8000)?;
    let bytes = &equipment.payload[3..23];
    let end = bytes
        .iter()
        .position(|b| *b == 0 || *b == 0xff)
        .unwrap_or(bytes.len());
    if bytes[..end] != *b"K155-11" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "K155-11 identity required",
        ));
    }
    Ok(())
}
