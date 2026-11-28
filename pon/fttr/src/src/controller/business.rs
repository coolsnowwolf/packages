//! Device-independent OMCI writes: preserve existing entities and require readback.
use crate::pon_engine::omci::{
    attributes::{AttributeCodec, AttributeContext, AttributeValue},
    error::{is_absent, is_unknown_instance, result_code},
    schema::MeRegistry,
    OmciSession,
};
use std::io;

pub fn matches(attributes: &[u8], expected: &[u8]) -> bool {
    attributes.get(..expected.len()) == Some(expected)
}
pub fn verify(
    session: &mut OmciSession,
    class: u16,
    instance: u16,
    mask: u16,
    expected: &[u8],
) -> io::Result<()> {
    if let Some(def) = MeRegistry::get(class).filter(|d| d.layout_supported) {
        let block = AttributeCodec::decode(def, mask, expected, AttributeContext::GetResponse);
        if block.undecoded_mask != 0 || block.consumed != expected.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid readback expectation",
            ));
        }
        let values: Vec<_> = block
            .attributes
            .into_iter()
            .map(|a| {
                (
                    a.definition.index,
                    if matches!(a.value, AttributeValue::Text { .. }) {
                        AttributeValue::octets(
                            &crate::pon_engine::omci::wire::unhex(&a.raw_hex).unwrap(),
                        )
                    } else {
                        a.value
                    },
                )
            })
            .collect();
        session.verify_attributes(class, instance, &values)?;
    } else if !matches(&session.get(class, instance, mask)?.payload[3..], expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("readback mismatch class={class} instance={instance:04x}"),
        ));
    }
    println!("READBACK_OK class={class} instance={instance:04x} mask={mask:04x}");
    Ok(())
}
pub fn create_verified(
    session: &mut OmciSession,
    class: u16,
    instance: u16,
    attributes: &[(u8, AttributeValue)],
    read_indices: &[u8],
) -> io::Result<()> {
    let expected = validate_create(class, attributes, read_indices)?;
    match session.read_attributes(class, instance, read_indices) {
        Ok(response) => {
            if !block_matches(&response, &expected) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("existing entity differs class={class} instance={instance:04x}"),
                ));
            }
            println!("EXISTING_READBACK_OK class={class} instance={instance:04x}");
            return Ok(());
        }
        Err(error) if is_unknown_instance(&error) => {}
        Err(error) => return Err(error),
    }
    session.create_attributes(class, instance, attributes)?;
    session.verify_attributes(class, instance, &expected)
}

/// Vendor-neutral CREATE. ONUs differ in what they will report and in which
/// result code they use for "already exists", so an unclear probe or a changing
/// entity is resolved by replacing the entity with our own definition instead of
/// aborting the profile forever.
pub fn create_verified_lenient(
    session: &mut OmciSession,
    class: u16,
    instance: u16,
    attributes: &[(u8, AttributeValue)],
    read_indices: &[u8],
) -> io::Result<()> {
    let expected = validate_create(class, attributes, read_indices)?;
    match session.read_attributes(class, instance, read_indices) {
        Ok(response) => {
            if block_matches(&response, &expected) {
                println!("EXISTING_READBACK_OK class={class} instance={instance:04x}");
                return Ok(());
            }
            println!("REPLACING_DIFFERING_ENTITY class={class} instance={instance:04x}");
            let _ = session.delete(class, instance);
        }
        Err(error) if is_absent(&error) => {}
        Err(error) if result_code(&error) == Some(9) => {
            // The ONU refuses this mask, so what is stored cannot be compared.
            println!("REPLACING_UNREPORTED_ENTITY class={class} instance={instance:04x}");
            let _ = session.delete(class, instance);
        }
        Err(error) => return Err(error),
    }
    match session.create_attributes(class, instance, attributes) {
        Ok(()) => {}
        Err(error) if matches!(result_code(&error), Some(4 | 7)) => {
            // Already there from an earlier attempt that we could not observe.
            let _ = session.delete(class, instance);
            session.create_attributes(class, instance, attributes)?;
        }
        Err(error) => return Err(error),
    }
    match session.verify_attributes(class, instance, &expected) {
        Ok(()) => Ok(()),
        Err(error) if is_absent(&error) || result_code(&error) == Some(9) => {
            println!("CREATE_ACCEPTED_WITHOUT_READBACK class={class} instance={instance:04x}");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn validate_create(
    class: u16,
    values: &[(u8, AttributeValue)],
    indices: &[u8],
) -> io::Result<Vec<(u8, AttributeValue)>> {
    let def = MeRegistry::get(class)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no complete ME layout"))?;
    // Validate the entire request before a lenient path can delete an entity.
    AttributeCodec::encode(def, values, AttributeContext::CreateRequest)?;
    AttributeCodec::read_mask(def, indices)?;
    read_expectations(values, indices)
}

fn read_expectations(
    values: &[(u8, AttributeValue)],
    indices: &[u8],
) -> io::Result<Vec<(u8, AttributeValue)>> {
    indices
        .iter()
        .map(|index| {
            values
                .iter()
                .find(|(i, _)| i == index)
                .cloned()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing readback value")
                })
        })
        .collect()
}
fn block_matches(
    block: &crate::pon_engine::omci::attributes::AttributeBlock,
    expected: &[(u8, AttributeValue)],
) -> bool {
    expected.iter().all(|(index, value)| {
        block
            .attributes
            .iter()
            .any(|a| a.definition.index == *index && a.value == *value)
    })
}

/// Standard Alloc-ID assignment. Instance and allocation come from the device plan.
pub fn assign_tcont(session: &mut OmciSession, instance: u16, alloc: u16) -> io::Result<()> {
    let before = session.read_attributes(262, instance, &[1])?;
    if before.attributes[0].value.unsigned() != Some(alloc.into()) {
        session.set_attributes(262, instance, &[(1, AttributeValue::number(alloc as u32))])?;
    }
    session.verify_attributes(262, instance, &[(1, AttributeValue::number(alloc.into()))])
}
pub fn create_gem(
    session: &mut OmciSession,
    instance: u16,
    port: u16,
    tcont: u16,
    bridge: u16,
    gal: u16,
) -> io::Result<()> {
    create_verified(session, 268, instance, &gem_ctp(port, tcont, 0), &[1, 2, 3])?;
    create_verified(
        session,
        266,
        instance,
        &gem_iw(instance, bridge, gal),
        &[1, 2, 3, 4, 7],
    )
}

// Configuration policy is explicit here; widths, masks and CREATE order belong to the schema.
fn numbers(values: &[(u8, u32)]) -> Vec<(u8, AttributeValue)> {
    values
        .iter()
        .map(|&(index, value)| (index, AttributeValue::number(value)))
        .collect()
}
pub fn gal() -> Vec<(u8, AttributeValue)> {
    numbers(&[(1, 48)])
}
pub fn bridge_profile() -> Vec<(u8, AttributeValue)> {
    // Keep the former zero-padded optional ageing time, rather than upstream's 300 default.
    numbers(&[
        (1, 0),
        (2, 1),
        (3, 0),
        (4, 0x8000),
        (5, 0x1400),
        (6, 0x0200),
        (7, 0x0f00),
        (8, 0),
        (9, 0),
        (10, 0),
    ])
}
pub fn bridge_port(bridge: u16, number: u8, tp: u8, pointer: u16) -> Vec<(u8, AttributeValue)> {
    numbers(&[
        (1, bridge.into()),
        (2, number.into()),
        (3, tp.into()),
        (4, pointer.into()),
        (5, 0),
        (6, 1),
        (7, 0),
        (8, 0),
        (9, 0),
        (13, 0),
        (14, 0),
    ])
}
pub fn evto(association: u8, pointer: u16) -> Vec<(u8, AttributeValue)> {
    numbers(&[(1, association.into()), (7, pointer.into()), (9, 0)])
}
pub fn vlan_filter() -> Vec<(u8, AttributeValue)> {
    vec![
        (1, AttributeValue::octets(&[0; 24])),
        (2, AttributeValue::number(0)),
        (3, AttributeValue::number(0)),
    ]
}
pub fn gem_ctp(port: u16, tcont: u16, upstream_tm: u16) -> Vec<(u8, AttributeValue)> {
    numbers(&[
        (1, port.into()),
        (2, tcont.into()),
        (3, 3),
        (4, upstream_tm.into()),
        (5, 0),
        (7, 0),
        (9, 0),
        (10, 0),
    ])
}
pub fn gem_iw(instance: u16, bridge: u16, gal: u16) -> Vec<(u8, AttributeValue)> {
    numbers(&[
        (1, instance.into()),
        (2, 1),
        (3, bridge.into()),
        (4, 0),
        (7, gal.into()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};

    struct NoTraffic;
    impl OmciTransport for NoTraffic {
        fn send(&mut self, _: &BaselineFrame) -> io::Result<()> {
            panic!("invalid request reached transport")
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            panic!("invalid request reached transport")
        }
    }
    #[test]
    fn invalid_create_cannot_probe_or_delete_an_existing_entity() {
        let mut session = OmciSession::new(NoTraffic);
        assert!(
            create_verified_lenient(&mut session, 272, 1, &numbers(&[(1, 65536)]), &[1]).is_err()
        );
        assert!(create_verified_lenient(&mut session, 272, 1, &gal(), &[1, 1]).is_err());
        assert!(create_verified(&mut session, 272, 1, &gal(), &[]).is_err());
        assert!(session
            .verify_attributes(272, 1, &numbers(&[(1, 65536)]))
            .is_err());
    }
    #[test]
    fn schema_create_payloads_preserve_legacy_zero_padded_wire() {
        // Independent pre-refactor payloads. Include the full 32-byte CREATE
        // area: optional upstream defaults must not change historical padding.
        for (class, values, legacy) in [
            (272, gal(), "0030"),
            (45, bridge_profile(), "0001008000140002000f0000"),
            (
                47,
                bridge_port(1, 6, 5, 0x106),
                "00010605010600000001000000",
            ),
            (171, evto(10, 0x401), "0a0401"),
            (
                84,
                vlan_filter(),
                "0000000000000000000000000000000000000000000000000000",
            ),
            (
                268,
                gem_ctp(0x106, 0x8000, 0),
                "010680000300000000000000000000",
            ),
            (
                268,
                gem_ctp(0x106, 0x8000, 0x8000),
                "010680000380000000000000000000",
            ),
            (266, gem_iw(0x106, 1, 1), "010601000100000001"),
        ] {
            let (_, mut actual) = AttributeCodec::encode(
                MeRegistry::get(class).unwrap(),
                &values,
                AttributeContext::CreateRequest,
            )
            .unwrap();
            let mut expected = crate::pon_engine::omci::wire::unhex(legacy).unwrap();
            actual.resize(32, 0);
            expected.resize(32, 0);
            assert_eq!(actual, expected, "class={class}");
        }
    }
    #[test]
    fn corrected_scheduler_and_uni_attributes_have_standard_masks_and_widths() {
        for (class, index, value, mask, bytes) in [
            (277, 7, 0x8000, 0x0200, vec![0x80, 0]),
            (11, 5, 0, 0x0800, vec![0]),
        ] {
            assert_eq!(
                AttributeCodec::encode(
                    MeRegistry::get(class).unwrap(),
                    &numbers(&[(index, value)]),
                    AttributeContext::SetRequest
                )
                .unwrap(),
                (mask, bytes)
            );
        }
    }
    #[test]
    fn rejects_short_and_different_readback_without_panicking() {
        assert!(!super::matches(&[1], &[1, 2]));
        assert!(!super::matches(&[1, 3], &[1, 2]));
        assert!(super::matches(&[1, 2, 0], &[1, 2]));
    }
}
