//! Compatibility entry for the standalone HL202 diagnostic.
pub fn configure(
    id: u8,
    seconds: Option<u64>,
    transparent: bool,
    supervised: bool,
    restore_initial: bool,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    #[cfg(feature = "device-hl202")]
    return crate::devices::hl202::service::configure(
        crate::platform::context::PlatformContext::detect().map_err(std::io::Error::other)?,
        id,
        seconds,
        transparent,
        supervised,
        restore_initial,
        cancellation,
    );
    #[cfg(not(feature = "device-hl202"))]
    {
        let _ = (
            id,
            seconds,
            transparent,
            supervised,
            restore_initial,
            cancellation,
        );
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "HL202 adapter not compiled",
        ))
    }
}
