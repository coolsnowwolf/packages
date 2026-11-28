//! Generic standard G.988 OMCI transparent bridge profile.
use super::inventory::BridgeInventory;
use crate::controller::business as me;
use crate::controller::business::{assign_tcont, create_verified_lenient, verify};
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::attributes::AttributeValue as V;
use crate::pon_engine::omci::OmciSession;
use crate::pon_engine::ploam::PloamFrame;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub fn configure(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    configure_with_options(platform, id, cancellation, None, None)
}

/// Explicit platform policy supplied by an opt-in adapter, under the same lock
/// and reversible lease as discovery and provisioning. Tuple is (management, business).
pub(crate) fn configure_with_grant_floors(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
    floors: Option<(u16, u16)>,
) -> io::Result<()> {
    configure_with_options(platform, id, cancellation, floors, None)
}

pub(crate) fn configure_with_evto(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
    configure_evto: Option<fn(&mut OmciSession, u16) -> io::Result<()>>,
) -> io::Result<()> {
    configure_with_options(platform, id, cancellation, None, configure_evto)
}

fn configure_with_options(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
    floors: Option<(u16, u16)>,
    configure_evto: Option<fn(&mut OmciSession, u16) -> io::Result<()>>,
) -> io::Result<()> {
    let resources = platform.resources().onu(id)?;
    let _lock = lock_onu(id)?;
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || crate::controller::service::operation_confirmed(id));
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    let mut lease = platform.management_lease(id)?;
    let _channel = platform.channel(id, 0)?;
    if let Some((management, business)) = floors {
        lease.set_management_grant_floor(management)?;
        lease.set_business_grant_floor(business)?;
    }

    let port = resources.gem_id;

    // Discover before writes: entity IDs are device-owned, not template constants.
    let mib = session.upload_mib()?;
    let owned_allocs: Vec<_> = platform
        .all_allocations()
        .iter()
        .map(|entry| entry.alloc_id)
        .collect();
    let inventory = BridgeInventory::from_mib(&mib, resources.alloc_id, &owned_allocs)?;
    let tcont_inst = inventory.tcont;
    println!(
        "GENERIC_BRIDGE_INVENTORY id={id} tcont={tcont_inst:04x} endpoints={:?} factory_ports={} factory_evtos={} used_port_nums={:?}",
        inventory.endpoints,
        inventory.ports.len(),
        inventory.evtos.len(),
        inventory.used_port_numbers
    );

    // The ONT keeps its MIB across daemon restarts and ONU-ID changes. Retire an
    // ANI binding that points at another slot's GEM port before creating ours;
    // otherwise this slot's GEM/ANI entities collide with the leftover ones.
    for &(instance, gem, _) in &inventory.gem_side {
        if gem == port
            || !platform
                .all_allocations()
                .iter()
                .any(|entry| entry.gem_id == gem)
        {
            continue;
        }
        println!(
            "GENERIC_STALE_ANI_RETIRED bridge_port={instance:04x} gem={gem:04x} this_slot={port:04x}"
        );
        let _ = session.delete(47, instance);
        let _ = session.delete(266, gem);
        let _ = session.delete(268, gem);
        let _ = session.delete(84, gem);
    }

    // Restore the initial, protocol-defined allocation before business setup.
    for _ in 0..3 {
        let _ = platform.send_ploam(&PloamFrame::assign_alloc_id(id));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Read ONU-G vendor ID
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN").to_vec();
    println!(
        "GENERIC_BRIDGE_START id={id} vendor={:?}",
        String::from_utf8_lossy(&vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported; ignore if rejected in SN mode
    let _ = session.set_attributes(256, 0, &[(12, V::number(1))]);

    // T-CONT allocation. Some ONUs refuse to rewrite Alloc-ID; the GEM port can
    // still be created against the existing T-CONT, so a refusal is not fatal.
    if let Err(error) = assign_tcont(&mut session, tcont_inst, resources.alloc_id) {
        println!(
            "GENERIC_TCONT_ALLOC_SKIPPED instance={tcont_inst:04x} alloc={:04x}: {error}",
            resources.alloc_id
        );
    }

    // GAL Ethernet profile (ME 272): reuse a factory profile when present.
    if let Err(error) = create_verified_lenient(&mut session, 272, 1, &me::gal(), &[1]) {
        println!("GENERIC_GAL_REUSED instance=0001: {error}");
    }

    // MAC Bridge Service Profile (ME 45)
    let bridge_inst = if session.get(45, 1, 0x8000).is_ok() {
        1u16
    } else if session.get(45, 0x200, 0x8000).is_ok() {
        0x200u16
    } else {
        1u16
    };
    if let Err(error) = create_verified_lenient(
        &mut session,
        45,
        bridge_inst,
        &me::bridge_profile(),
        &[1, 2, 3],
    ) {
        println!("GENERIC_BRIDGE_PROFILE_REUSED instance={bridge_inst:04x}: {error}");
    }

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem_port(&mut session, port, port, tcont_inst, bridge_inst, 1)?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP); never reuse a taken PortNum.
    // Reuse the PortNum this slot's ANI port already has, otherwise the
    // readback comparison changes between runs and the profile never settles.
    let ani_port_num = match inventory
        .gem_side
        .iter()
        .find(|(_, gem, _)| *gem == port)
        .map(|(_, _, number)| *number)
    {
        Some(number) if number != 0 => number,
        _ => [6u8, 7, 8, 9, 10, 11, 12, 13, 14, 15]
            .into_iter()
            .find(|number| !inventory.used_port_numbers.contains(number))
            .unwrap_or(6),
    };
    create_verified_lenient(
        &mut session,
        47,
        port,
        &me::bridge_port(bridge_inst, ani_port_num, 5, port),
        &[1, 2, 3, 4],
    )?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified_lenient(&mut session, 84, port, &me::vlan_filter(), &[1, 2]);

    let mut used_port_numbers = inventory.used_port_numbers.clone();
    used_port_numbers.push(ani_port_num);
    for &(endpoint, is_veip) in &inventory.endpoints {
        // Reuse the ONT's own bridge port when it is already bound to our bridge:
        // a second port for the same UNI both collides on PortNum and replaces
        // the vendor VLAN/EVTO plan that carries the ONT's management address.
        let bp_inst = match inventory
            .ports
            .get(&endpoint)
            .copied()
            .filter(|existing| existing.bridge == bridge_inst)
        {
            Some(existing) => {
                used_port_numbers.push(existing.port_num);
                println!(
                    "GENERIC_ENDPOINT_PORT_REUSED endpoint={endpoint:04x} instance={:04x} port_num={}",
                    existing.instance, existing.port_num
                );
                existing.instance
            }
            None => {
                let port_num = [1u8, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15]
                    .into_iter()
                    .find(|number| !used_port_numbers.contains(number))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::AlreadyExists, "no free bridge port number")
                    })?;
                let instance = 0x200u16 + u16::from(port_num);
                create_verified_lenient(
                    &mut session,
                    47,
                    instance,
                    &me::bridge_port(
                        bridge_inst,
                        port_num,
                        if is_veip { 11 } else { 1 },
                        endpoint,
                    ),
                    &[1, 2, 3, 4],
                )?;
                used_port_numbers.push(port_num);
                instance
            }
        };

        // Reuse the EVTO already associated with this endpoint instead of adding
        // a second one for the same UNI.
        let evto_inst = match inventory.evtos.get(&endpoint).copied() {
            Some(instance) => {
                println!(
                    "GENERIC_ENDPOINT_EVTO_REUSED endpoint={endpoint:04x} instance={instance:04x}"
                );
                instance
            }
            None => {
                create_verified_lenient(
                    &mut session,
                    171,
                    bp_inst,
                    &me::evto(if is_veip { 10 } else { 2 }, endpoint),
                    &[1, 7],
                )?;
                bp_inst
            }
        };
        match session.set_attributes(171, evto_inst, &[(5, V::number(1))]) {
            Ok(()) => {
                let _ = verify(&mut session, 171, evto_inst, 0x0800, &[1]);
            }
            Err(error) => {
                println!("GENERIC_EVTO_DOWNSTREAM_MODE_SKIPPED instance={evto_inst:04x}: {error}")
            }
        }
        if let Some(configure_evto) = configure_evto {
            session.with_timeout(std::time::Duration::from_secs(30), |session| {
                configure_evto(session, evto_inst)
            })?;
        } else if let Err(error) = session
            .with_timeout(std::time::Duration::from_secs(30), |session| {
                apply_transparent_evto_table(session, evto_inst)
            })
        {
            if matches!(
                error.kind(),
                io::ErrorKind::NotConnected | io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
            println!("GENERIC_EVTO_TABLE_SKIPPED instance={evto_inst:04x}: {error}");
        }
        if is_veip {
            crate::controller::management::bootstrap(&mut session, id, endpoint);
        }
    }

    verify(&mut session, 256, 0, 0x8000, &vendor_bytes)?;
    println!(
        "GENERIC_BRIDGE_SERVICE_READBACK_OK gem={port:04x} endpoints={:?}; data evidence still required",
        inventory.endpoints
    );
    lease.disarm()?;
    println!("GENERIC_LEASE_PERSISTED id={id}");
    Ok(())
}

/// ME 268 for a third-party ONU. The upstream traffic-management pointer
/// (attribute 4) must reference the T-CONT that carries the GEM port: a zero
/// pointer is rejected with result 3 (parameter error, attribute mask 0x1000)
/// by ITU-T conformant OMCI implementations, e.g. Lantiq/Huawei based ONTs
/// ("SBC validation error (mask 0x1000)" in the ONU daemon log). With the
/// T-CONT pointer the ONU treats the upstream attribute as redundant and
/// accepts the CREATE.
fn create_gem_port(
    session: &mut OmciSession,
    instance: u16,
    port: u16,
    tcont: u16,
    bridge: u16,
    gal: u16,
) -> io::Result<()> {
    create_verified_lenient(
        session,
        268,
        instance,
        &me::gem_ctp(port, tcont, tcont),
        &[1, 2, 3],
    )?;
    // Earlier firmware created this GEM port with TM pointer 0. Repair it when
    // the ONU lets us read the attribute; ignore ONUs that reject the mask.
    match session.get(268, instance, 0x1000) {
        Ok(frame) if frame.payload[3..5] != tcont.to_be_bytes() => {
            session.set_attributes(268, instance, &[(4, V::number(tcont.into()))])?;
            println!("GEM_UPSTREAM_TM_REPAIRED instance={instance:04x} tcont={tcont:04x}");
        }
        Ok(_) => {}
        Err(_) => {}
    }
    create_verified_lenient(
        session,
        266,
        instance,
        &me::gem_iw(instance, bridge, gal),
        &[1, 2, 3, 4, 7],
    )
}

fn apply_transparent_evto_table(session: &mut OmciSession, instance: u16) -> io::Result<()> {
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
                "EVTO default table readback differs",
            ));
        }
        println!("GENERIC_EVTO_TRANSPARENT_OK instance={instance:04x}");
    } else {
        println!("GENERIC_EXISTING_EVTO_PRESERVED instance={instance:04x}");
    }
    Ok(())
}
