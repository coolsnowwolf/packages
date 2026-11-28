use crate::controller::business::verify;
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
        super::super::Profile::V152,
    )
}

pub(in crate::devices::hwtc) fn verify_identity(session: &mut OmciSession) -> io::Result<()> {
    let equipment = session.get(257, 0, 0x8000)?;
    let bytes = &equipment.payload[3..23];
    let end = bytes
        .iter()
        .position(|b| *b == 0 || *b == 0xff)
        .unwrap_or(bytes.len());
    if bytes[..end] != *b"V152" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "V152 identity required",
        ));
    }
    Ok(())
}

pub(in crate::devices::hwtc) fn after_bridge(session: &mut OmciSession) -> io::Result<()> {
    // VEIP is ONU-created: unlock, never Create class 329.
    let before = session.get(329, 0x601, 0x8000)?;
    println!("V152_VEIP_ADMIN_BEFORE={}", before.payload[3]);
    if before.payload[3] != 0 {
        session.set_attributes(
            329,
            0x601,
            &[(
                1,
                crate::pon_engine::omci::attributes::AttributeValue::number(0),
            )],
        )?;
    }
    verify(session, 329, 0x601, 0x8000, &[0])?;
    // CurrentAddress is 0080; 00f8 includes current IP/mask/gateway/DNS.
    let mut probes = vec![
        (257, 0, 0x0038, 5usize),
        (131, 0, 0x8000, 4),
        (329, 0x601, 0x4000, 1),
        (266, 0x200, 0x0400, 1),
    ];
    for host in 0..4 {
        probes.extend([(134, host, 0xc000, 7), (134, host, 0x00f8, 20)]);
    }
    for (class, instance, mask, length) in probes {
        match session.get(class, instance, mask) {
            Ok(frame) => println!(
                "V152_STATE class={class} instance={instance:04x} mask={mask:04x} data={:02X?}",
                &frame.payload[3..3 + length]
            ),
            Err(e) => println!(
                "V152_STATE class={class} instance={instance:04x} mask={mask:04x} unavailable: {e}"
            ),
        }
    }
    // ME 288 is excluded from MIB upload; stop if the control query fails.
    match session.get(288, 340, 0xa100) {
        Ok(frame) => {
            log_descriptor(340, &frame.payload);
            for class in [
                130, 134, 266, 329, 65331, 65347, 65352, 65355, 65360, 65362, 65394, 65457, 65458,
                65528, 65529,
            ] {
                match session.get(288, class, 0xa100) {
                    Ok(frame) => log_descriptor(class, &frame.payload),
                    Err(e) => println!("V152_ME_DESCRIPTOR class={class} unavailable: {e}"),
                }
            }
        }
        Err(e) => println!("V152_ME_DESCRIPTORS unavailable: {e}"),
    }
    println!("V152_UNTAGGED_BRIDGE_READBACK_OK; WAN bootstrap pending: no verified INTERNET/TR069 creation contract; data traffic not yet verified");
    Ok(())
}

fn log_descriptor(class: u16, payload: &[u8; 32]) {
    let name = String::from_utf8_lossy(&payload[3..28]);
    println!(
        "V152_ME_DESCRIPTOR class={class} name={:?} access={} support={}",
        name.trim_end_matches('\0'),
        payload[28],
        payload[29]
    );
}
