#[cfg(feature = "device-hwtc")]
fn main() -> std::io::Result<()> {
    let id = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: fttr-k155-service ONU_ID",
            )
        })?;
    let platform = openwrt_fttr::platform::context::PlatformContext::detect()
        .map_err(std::io::Error::other)?;
    openwrt_fttr::devices::hwtc::k155::service::configure(platform, id, None)
}
#[cfg(not(feature = "device-hwtc"))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "HWTC adapter not compiled",
    ))
}
