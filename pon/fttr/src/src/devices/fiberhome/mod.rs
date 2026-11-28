//! Explicit opt-in FiberHome bridges; no automatic enrollment.
mod hg6371f;
mod hg6371f_c;
mod service;

pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "FHTT" && equipment.is_some_and(|e| e == "HG6371F" || e.starts_with("HG6371F-"))
}

pub(super) fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    let mut session = crate::pon_engine::omci::OmciSession::new(platform.transport(id)?);
    let equipment = {
        let _lock = crate::platform::openwrt::onu_lock::lock_onu(id)?;
        let _lease = platform.management_lease(id)?;
        let _channel = platform.channel(id, 0)?;
        session.guard_registration(move || crate::controller::service::operation_confirmed(id));
        if let Some(token) = &cancellation {
            session.cancel_when(token.clone());
        }
        crate::controller::business::verify(&mut session, 256, 0, 0x8000, b"FHTT")?;
        let block = session.get_attributes(257, 0, 0x8000)?;
        block
            .attributes
            .first()
            .and_then(|a| a.value.text())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    if equipment == "HG6371F" {
        hg6371f::configure(platform, id, cancellation)
    } else if equipment.starts_with("HG6371F-") {
        hg6371f_c::configure(platform, id, cancellation)
    } else {
        service::configure(platform, id, cancellation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_vendor_and_hg6371f_prefix() {
        assert!(matches("FHTT", Some("HG6371F-C")));
        assert!(matches("FHTT", Some("HG6371F-G-C")));
        assert!(matches("FHTT", Some("HG6371F-X")));
        assert!(!matches("SCTY", Some("HG6371F-C")));
        assert!(matches("FHTT", Some("HG6371F")));
        assert!(!matches("FHTT", Some("HG6371FX")));
        assert!(!matches("FHTT", None));
        let identity = crate::model::OnuIdentity {
            equipment_id: Some("HG6371F-G-C".into()),
            ..Default::default()
        };
        assert_eq!(
            crate::devices::select("FHTT", &identity),
            crate::devices::Adapter::Fiberhome
        );
    }
}
