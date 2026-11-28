//! F650A keeps factory discard defaults in its VEIP EVTO table.
use crate::pon_engine::omci::OmciSession;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub fn matches(equipment: Option<&str>) -> bool {
    equipment == Some("ZXHN F650A(GPON ONU)")
}

fn default_table(discard: bool) -> Vec<u8> {
    [
        (0xf8000000u32, 0xf8000000u32),
        (0xf8000000, 0xe8000000),
        (0xe8000000, 0xe8000000),
    ]
    .into_iter()
    .flat_map(|(outer, inner)| {
        [
            outer,
            inner,
            if discard { 0xff0fffff } else { 0x000f0000 },
            if discard { 0xff0fffff } else { 0x000f0000 },
        ]
        .into_iter()
        .flat_map(u32::to_be_bytes)
    })
    .collect()
}

fn needs_replacement(before: &[u8]) -> io::Result<bool> {
    if before == default_table(false) {
        Ok(false)
    } else if before.is_empty() || before == default_table(true) {
        Ok(true)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "F650A EVTO is not the known factory discard or transparent table; refusing to replace",
        ))
    }
}

fn configure_evto(session: &mut OmciSession, instance: u16) -> io::Result<()> {
    let before = session.get_table(171, instance, 0x0400)?;
    if !needs_replacement(&before)? {
        println!("F650A_EVTO_TRANSPARENT_READBACK_OK instance={instance:04x}");
        return Ok(());
    }
    let expected = default_table(false);
    for row in expected.chunks_exact(16) {
        session.set(171, instance, 0x0400, row)?;
    }
    if session.get_table(171, instance, 0x0400)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "F650A EVTO transparent table readback differs",
        ));
    }
    println!(
        "F650A_EVTO_FACTORY_DISCARD_REPLACED instance={instance:04x}; data evidence still required"
    );
    Ok(())
}

pub fn configure(
    platform: crate::platform::context::PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    crate::devices::generic::service::configure_with_evto(
        platform,
        id,
        cancellation,
        Some(configure_evto),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::tables::VlanRule;

    #[test]
    fn replaces_only_exact_factory_defaults_and_is_idempotent() {
        assert!(needs_replacement(&[]).unwrap());
        assert!(needs_replacement(&default_table(true)).unwrap());
        assert!(!needs_replacement(&default_table(false)).unwrap());
        let mut custom = default_table(true);
        custom[1] = 1;
        assert!(needs_replacement(&custom).is_err());
        assert!(needs_replacement(&custom[..32]).is_err());
    }

    #[test]
    fn transparent_defaults_preserve_filters_without_discard_or_tag_rewrite() {
        let discard = default_table(true);
        let transparent = default_table(false);
        assert_eq!(transparent.len(), 48);
        for (old, new) in discard.chunks_exact(16).zip(transparent.chunks_exact(16)) {
            assert_eq!(&old[..8], &new[..8]);
            assert_eq!(VlanRule::decode(old).unwrap().tags_to_remove, 3);
            let rule = VlanRule::decode(new).unwrap();
            assert_eq!(rule.tags_to_remove, 0);
            assert_eq!(rule.treatment_outer_priority, 15);
            assert_eq!(rule.treatment_inner_priority, 15);
            assert!(!rule.delete_entry);
        }
    }
}
