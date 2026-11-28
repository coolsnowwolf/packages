//! Realtek (RTL) platform profiles.
mod tyfca1;

pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    matches!(vendor, "D05B" | "TYZD") && equipment == Some("TYFCA1")
}

pub(super) fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    tyfca1::configure(platform, id, cancellation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_tyfca1() {
        assert!(matches("D05B", Some("TYFCA1")));
        assert!(matches("TYZD", Some("TYFCA1")));
        assert!(!matches("UNKN", Some("TYFCA1")));
        assert!(!matches("D05B", Some("TY-FS803G")));
    }
}
