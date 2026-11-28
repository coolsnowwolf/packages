//! Shared bridge topology, initially verified on K155-11. Not a WAN creation contract.
use crate::controller::business as me;
use crate::controller::business::{assign_tcont, create_verified, verify};
use crate::pon_engine::omci::attributes::AttributeValue as V;
use crate::pon_engine::omci::OmciSession;
use std::io;

pub(super) fn configure(session: &mut OmciSession, port: u16, alloc: u16) -> io::Result<()> {
    let entity = 0x200u16;
    create_verified(session, 272, 1, &me::gal(), &[1])?;
    create_verified(session, 45, entity, &me::bridge_profile(), &[1, 2, 3])?;
    assign_tcont(session, 0x8000, alloc)
        .inspect_err(|error| eprintln!("HWTC_TCONT_ASSIGN_FAILED instance=8000: {error}"))?;
    // Stable instances survive ONU-ID changes; queue 8000 belongs to T-CONT 8000.
    // K155 rejects GET of direction (2000). Read supported pointers separately.
    let mut ctp_readback = Vec::new();
    ctp_readback.extend(port.to_be_bytes());
    ctp_readback.extend([0x80, 0, 0x80, 0, 0, 0]);
    match session.get(268, entity, 0x8000) {
        Ok(port_frame) => {
            let frame = session.get(268, entity, 0x5000).inspect_err(|error| {
                eprintln!("HWTC_CTP_POINTER_GET_FAILED instance={entity:04x}: {error}")
            })?;
            if frame.payload[3..7] != [0x80, 0, 0x80, 0] {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "existing HWTC GEM topology differs",
                ));
            }
            if u16::from_be_bytes([port_frame.payload[3], port_frame.payload[4]]) != port {
                session
                    .set_attributes(268, entity, &[(1, V::number(port.into()))])
                    .inspect_err(|error| {
                        eprintln!("HWTC_CTP_PORT_SET_FAILED instance={entity:04x}: {error}")
                    })?;
            }
            verify(session, 268, entity, 0xd200, &ctp_readback).inspect_err(|error| {
                eprintln!("HWTC_CTP_VERIFY_FAILED instance={entity:04x}: {error}")
            })?;
        }
        Err(e) if crate::pon_engine::omci::error::is_unknown_instance(&e) => {
            session.create_attributes(268, entity, &me::gem_ctp(port, 0x8000, 0x8000))?;
            verify(session, 268, entity, 0xd200, &ctp_readback)?;
        }
        Err(e) => {
            eprintln!("HWTC_CTP_PORT_GET_FAILED instance={entity:04x}: {e}");
            return Err(e);
        }
    }
    create_verified(
        session,
        266,
        entity,
        &me::gem_iw(entity, entity, 1),
        &[1, 2, 3, 4, 7],
    )?;
    for (instance, number, tp, pointer) in [
        (0x201u16, 1u8, 1u8, 0x101u16),
        (0x202, 2, 1, 0x102),
        (0x205, 5, 11, 0x601),
        (entity, 6, 5, entity),
    ] {
        create_verified(
            session,
            47,
            instance,
            &me::bridge_port(entity, number, tp, pointer),
            &[1, 2, 3, 4],
        )?;
        if tp == 5 {
            continue;
        }
        let association = if tp == 11 { 10 } else { 2 };
        create_verified(
            session,
            171,
            instance,
            &me::evto(association, pointer),
            &[1, 7],
        )?;
        session.set_attributes(171, instance, &[(5, V::number(1))])?;
        verify(session, 171, instance, 0x0800, &[1])?;
        let before = session.get_table(171, instance, 0x0400)?;
        if before.is_empty() {
            let mut expected = Vec::new();
            for (outer, inner) in [
                (0xf8000000u32, 0xf8000000u32),
                (0xf8000000, 0xe8000000),
                (0xe8000000, 0xe8000000),
            ] {
                let row: Vec<u8> = [outer, inner, 0x000f0000, 0x000f0000]
                    .into_iter()
                    .flat_map(u32::to_be_bytes)
                    .collect();
                session.set_attributes(171, instance, &[(6, V::octets(&row))])?;
                expected.extend(row);
            }
            if session.get_table(171, instance, 0x0400)? != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HWTC EVTO readback differs",
                ));
            }
        } else {
            println!("HWTC_EXISTING_EVTO_PRESERVED instance={instance:04x}");
        }
    }
    create_verified(session, 84, entity, &me::vlan_filter(), &[1, 2])?;
    Ok(())
}
