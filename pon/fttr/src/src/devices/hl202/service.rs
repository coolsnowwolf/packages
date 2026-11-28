//! Verified HL202 service configuration; verify each accepted write by GET.
use crate::controller::business as me;
use crate::controller::business::{assign_tcont, create_gem, create_verified, verify};
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::attributes::AttributeValue as V;
use crate::pon_engine::omci::{error::is_unknown_instance, OmciSession};
use crate::pon_engine::ploam::PloamFrame;
use std::io;

// Do not enable the FPGA ONU/OMCC mapping while discovery/ranging owns it.
// Query the live daemon, never the CLI's cached status-file fallback.
fn wait_for_operation(id: u8) {
    use crate::{ipc, model::FttrSystemStatus};
    let mut attempts = 0u64;
    loop {
        let ready = ipc::send_ipc_command("STATUS")
            .ok()
            .and_then(|reply| serde_json::from_str::<FttrSystemStatus>(&reply).ok())
            .is_some_and(|status| {
                status.sub_gateways.iter().any(|onu| {
                    onu.onu_id == id
                        && onu.vendor == "H3CT"
                        && onu.state == crate::pon_engine::ploam::OnuState::O5Operation
                })
            });
        if ready {
            println!("ONU_OPERATION_CONFIRMED id={id}; starting OMCI service");
            return;
        }
        if attempts % 10 == 0 {
            println!("WAITING_FOR_O5 id={id}; OMCC mapping untouched");
        }
        attempts += 1;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn retire_previous_ani(
    platform: PlatformContext,
    session: &mut OmciSession,
    current: u16,
) -> io::Result<()> {
    // ONU-ID can change across daemon/main restarts while the child retains
    // its MIB. Two ANI bridge entries with the same port number leave the
    // old GEM selected. Retire ONLY entities matching our verified profile.
    for old in platform.all_allocations().iter().map(|entry| entry.gem_id) {
        if old == current {
            continue;
        }
        let entry = match session.get(47, old, 0xf000) {
            Ok(entry) => entry,
            Err(error) if is_unknown_instance(&error) => continue,
            Err(error) => return Err(error),
        };
        let expected = [0, 1, 6, 5, (old >> 8) as u8, old as u8];
        if entry.payload[3..9] != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("foreign ANI bridge profile {old:04x}; migration blocked"),
            ));
        }
        let mut entities = vec![(47, old)];
        for (class, mask, expected) in [
            (
                266,
                0xf200,
                vec![(old >> 8) as u8, old as u8, 1, 0, 1, 0, 0, 0, 1],
            ),
            (268, 0xe000, vec![(old >> 8) as u8, old as u8, 0x80, 0, 3]),
            (84, 0xc000, vec![0; 25]),
        ] {
            match session.get(class, old, mask) {
                Ok(entry) if entry.payload[3..3 + expected.len()] == expected => {
                    entities.push((class, old))
                }
                Err(error) if is_unknown_instance(&error) => {}
                Err(error) => return Err(error),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "foreign entity class={class} instance={old:04x}; migration blocked"
                        ),
                    ))
                }
            }
        }
        for (class, instance) in entities {
            session.delete(class, instance)?;
            match session.get(class, instance, if class == 84 { 0xc000 } else { 0x8000 }) {
                Err(error) if is_unknown_instance(&error) => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retired ANI entity still present or GET failed",
                    ))
                }
            }
            println!(
                "PREVIOUS_ANI_RETIRED class={class} instance={instance:04x} new={current:04x}"
            );
        }
    }
    Ok(())
}
pub fn configure(
    platform: PlatformContext,
    id: u8,
    seconds: Option<u64>,
    transparent: bool,
    supervised: bool,
    restore_initial: bool,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> io::Result<()> {
    platform.resources().onu(id)?;
    if seconds.is_some_and(|s| s > 600) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hold seconds must be <=600",
        ));
    }
    if supervised && cancellation.is_none() {
        wait_for_operation(id);
    }
    if cancellation
        .as_ref()
        .is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire))
    {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "registration cancelled before configuration",
        ));
    }
    // Declared before all hardware leases: released only after their Drop.
    let _operation_lock = lock_onu(id)?;
    if supervised && !crate::controller::service::operation_confirmed(id) {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "ONU went offline before OMCC setup",
        ));
    }
    let mut session = OmciSession::new(platform.transport(id)?);
    if supervised {
        session.guard_registration(move || crate::controller::service::operation_confirmed(id));
    }
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    let mut lease = platform.management_lease(id)?;
    let port = platform.resources().onu(id)?.gem_id;
    let _channel = platform.channel(id, 0)?;
    if restore_initial {
        // Assign ONU-ID, not the data T-CONT Alloc-ID.
        // Explicit recovery of earlier diagnostics; require a real GET below.
        for _ in 0..3 {
            platform.send_ploam(&PloamFrame::assign_alloc_id(id))?;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let mut channel = 0;
    // Alloc-ID is retained by the ONU when a diagnostic lease exits, while
    // the host mapping is restored. Recover the existing channel before SET.
    match session.get(256, 0, 0x8000) {
        Ok(frame) if &frame.payload[3..7] == b"H3CT" => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected ONU vendor",
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            platform.exchange_channel(id, port)?;
            channel = port;
            verify(&mut session, 256, 0, 0x8000, b"H3CT")?;
            println!("EXISTING_ALLOC_CHANNEL_RECOVERED channel={port:04x}");
        }
        Err(e) => return Err(e),
    }
    let equipment = session.get(257, 0, 0x8000)?;
    let equipment = &equipment.payload[3..23];
    let end = equipment
        .iter()
        .position(|byte| *byte == 0 || *byte == 0xff)
        .unwrap_or(equipment.len());
    if std::str::from_utf8(&equipment[..end]).map(str::trim) != Ok("HL202-DU-C") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "HL202 profile requires verified HL202-DU-C equipment identity",
        ));
    }
    // Authorization is performed by the adapter entry before business writes.
    crate::omci_auth::admit(id, || Ok(()))?;
    // Assign the platform-owned Alloc-ID to the model's T-CONT 0x8000.
    // Create GAL profile 1 and bridge profile 1 first.
    create_verified(&mut session, 272, 1, &me::gal(), &[1])?;
    create_verified(&mut session, 45, 1, &me::bridge_profile(), &[1, 2, 3])?;
    assign_tcont(&mut session, 0x8000, platform.resources().onu(id)?.alloc_id)?;
    // Use the platform-owned GEM-ID, bidirectional, with queue pointers 0.
    // Bind MAC-bridged GEM interworking option 1 to bridge 1/GAL 1.
    create_gem(&mut session, port, port, 0x8000, 1, 1)?;
    retire_previous_ani(platform, &mut session, port)?;
    // First Ethernet UNI from the actual MIB is 0x0101, not ONU-ID.
    create_verified(
        &mut session,
        47,
        1,
        &me::bridge_port(1, 1, 1, 0x101),
        &[1, 2, 3, 4],
    )?;
    if session.get(47, port, 0x4000).is_ok() {
        session.set_attributes(47, port, &[(2, V::number(6))])?;
    }
    create_verified(
        &mut session,
        47,
        port,
        &me::bridge_port(1, 6, 5, port),
        &[1, 2, 3, 4],
    )?;
    for index in 2..=4u16 {
        let pointer = 0x100 + index;
        create_verified(
            &mut session,
            47,
            index,
            &me::bridge_port(1, index as u8, 1, pointer),
            &[1, 2, 3, 4],
        )?;
    }
    // Bridge the MIB-discovered VEIP using TP type 11.
    session.set_attributes(47, port, &[(2, V::number(6))])?;
    verify(&mut session, 47, port, 0x4000, &[6])?;
    create_verified(
        &mut session,
        47,
        5,
        &me::bridge_port(1, 5, 11, 0x401),
        &[1, 2, 3, 4],
    )?;
    for index in 1..=5u16 {
        let association = if index == 5 { 10 } else { 2 };
        let pointer = if index == 5 { 0x401u16 } else { 0x100 + index };
        create_verified(
            &mut session,
            171,
            index,
            &me::evto(association, pointer),
            &[1, 7],
        )?;
        // Use downstream transparent forwarding; do not guess a VLAN rule.
        session.set_attributes(171, index, &[(5, V::number(1))])?;
        verify(&mut session, 171, index, 0x0800, &[1])?;
        if transparent {
            let before = session.get_table(171, index, 0x0400)?;
            if before.is_empty() {
                let mut expected = Vec::new();
                for (outer, inner) in [
                    (0xf8000000u32, 0xf8000000u32),
                    (0xf8000000, 0xe8000000),
                    (0xe8000000, 0xe8000000),
                ] {
                    let mut row = Vec::new();
                    for word in [outer, inner, 0x000f0000, 0x000f0000] {
                        row.extend(word.to_be_bytes());
                    }
                    session.set_attributes(171, index, &[(6, V::octets(&row))])?;
                    expected.extend(row);
                }
                let actual = session.get_table(171, index, 0x0400)?;
                if actual != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "EVTO default rows readback differs",
                    ));
                }
                println!(
                    "EVTO_TRANSPARENT_DEFAULTS_READBACK_OK instance={index} bytes={}",
                    actual.len()
                );
            } else {
                println!(
                    "EVTO_EXISTING_TABLE_PRESERVED instance={index} bytes={}",
                    before.len()
                );
            }
        }
    }
    // Complete the ANI bridge with class 84, using the selected GEM instance:
    // zero VLAN entries and forward operation 0.
    // Preserve any existing profile that differs rather than replacing it.
    // This HL202 rejects GET of the entry-count attribute (mask 0x2000,
    // result 9 with unsupported-mask 0x2000). Validate the supported list
    // and forward-operation attributes; do not treat the rejected GET as success.
    create_verified(&mut session, 84, port, &me::vlan_filter(), &[1, 2])?;
    println!("STOCK_ANI_VLAN_FILTER_READBACK_OK instance={port:04x}; entry-count GET unsupported");
    println!("RESULT OMCI_service_entities_readback=true data_plane_verified=false");
    crate::controller::management::bootstrap(&mut session, id, 0x401);
    for (class, instance, mask) in [
        (329, 0x401, 0xc000),
        (11, 0x101, 0x0c00),
        (11, 0x102, 0x0c00),
        (171, 5, 0x0400),
        (65530, 0, 0x8000),
    ] {
        match session.get(class, instance, mask) {
            Ok(frame) => println!(
                "SERVICE_STATE class={class} instance={instance:04x} mask={mask:04x} data={:02X?}",
                &frame.payload[3..]
            ),
            Err(e) => eprintln!("SERVICE_STATE class={class} instance={instance:04x}: {e}"),
        }
    }
    if let Some(seconds) = seconds {
        // T-CONT 0x8000 has already been verified above. Do not reassign the
        // initial/OMCC Alloc-ID to the business Alloc-ID just to hold the lease.
        verify(&mut session, 256, 0, 0x8000, b"H3CT")?;
        println!("SERVICE_OMCI_READBACK_OK channel={channel:04x}");
        println!("STOCK_DATA_ALLOC_ID {port:04x}; holding verified OMCC setup for {seconds}s");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < until {
            std::thread::sleep(
                std::time::Duration::from_secs(5)
                    .min(until.saturating_duration_since(std::time::Instant::now())),
            );
            verify(&mut session, 256, 0, 0x8000, b"H3CT")?;
            println!("OMCC_HEARTBEAT_OK channel={channel:04x}; data plane still requires packet evidence");
        }
    }
    lease.disarm()?;
    println!("HL202_LEASE_PERSISTED id={id}");
    Ok(())
}
