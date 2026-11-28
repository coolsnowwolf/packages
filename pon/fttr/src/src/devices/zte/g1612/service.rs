//! ZXHN G1612 specific OMCI transparent bridge profile.
//!
//! Direct OMCI provisioning bypasses the costly 351-entity MIB upload
//! on ZTE protocol stacks which produce 27ms duplicate frames and socket timeouts.
//! Known hardware profile:
//! - T-CONT instance: 0x8002 (32770)
//! - VEIP instance: 0x0301 (769)
//! - Bridge Port instance: 0x0201 (port_num 1)
//! - EVTO instance: 0x0201
//! - ME 340 (TR-069) instance: 0x0301
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

pub const G1612_TCONT: u16 = 0x8002;
pub const G1612_VEIP_ENDPOINT: u16 = 0x0301;
pub const G1612_VEIP_BP: u16 = 0x0201;
pub const G1612_VEIP_EVTO: u16 = 0x0201;
pub const G1612_BRIDGE_PROFILE: u16 = 1;
pub const G1612_GAL_PROFILE: u16 = 1;

pub fn configure(
    platform: PlatformContext,
    id: u8,
    cancellation: Option<Arc<AtomicBool>>,
) -> io::Result<()> {
    platform.resources().onu(id)?;
    let _lock = lock_onu(id)?;
    let mut session = OmciSession::new(platform.transport(id)?);
    session.guard_registration(move || crate::controller::service::operation_confirmed(id));
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    let mut lease = platform.management_lease(id)?;
    let _channel = platform.channel(id, 0)?;

    let resources = platform.resources().onu(id)?;
    let port = resources.gem_id;
    let alloc = resources.alloc_id;

    println!("ZTE_G1612_PROVISION_START id={id} port={port:04x}");

    // Send Assign_Alloc-ID downstream PLOAM
    for _ in 0..3 {
        let _ = platform.send_ploam(&PloamFrame::assign_alloc_id(id));
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Verify vendor
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN").to_vec();
    println!(
        "ZTE_G1612_VENDOR id={id} vendor={:?}",
        String::from_utf8_lossy(&vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported
    let _ = session.set_attributes(256, 0, &[(12, V::number(1))]);

    // Retire stale ANI/GEM entries from other slots
    retire_stale_ani(platform, &mut session, port);

    // T-CONT allocation to 0x8002
    if let Err(error) = assign_tcont(&mut session, G1612_TCONT, alloc) {
        println!(
            "ZTE_G1612_TCONT_ALLOC_SKIPPED instance={G1612_TCONT:04x} alloc={alloc:04x}: {error}"
        );
    }

    // GAL Ethernet profile (ME 272)
    if let Err(error) =
        create_verified_lenient(&mut session, 272, G1612_GAL_PROFILE, &me::gal(), &[1])
    {
        println!("ZTE_G1612_GAL_REUSED instance={G1612_GAL_PROFILE:04x}: {error}");
    }

    // MAC Bridge Service Profile (ME 45)
    if let Err(error) = create_verified_lenient(
        &mut session,
        45,
        G1612_BRIDGE_PROFILE,
        &me::bridge_profile(),
        &[1, 2, 3],
    ) {
        println!("ZTE_G1612_BRIDGE_PROFILE_REUSED instance={G1612_BRIDGE_PROFILE:04x}: {error}");
    }

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem_port(
        &mut session,
        port,
        port,
        G1612_TCONT,
        G1612_BRIDGE_PROFILE,
        G1612_GAL_PROFILE,
    )?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP, port_num=6)
    let ani_port_num = 6u8;
    create_verified_lenient(
        &mut session,
        47,
        port,
        &me::bridge_port(G1612_BRIDGE_PROFILE, ani_port_num, 5, port),
        &[1, 2, 3, 4],
    )?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified_lenient(&mut session, 84, port, &me::vlan_filter(), &[1, 2]);

    // Check or create VEIP Bridge Port (ME 47 instance 0x0201)
    let bp_inst = G1612_VEIP_BP;
    if session.get(47, bp_inst, 0x8000).is_err() {
        create_verified_lenient(
            &mut session,
            47,
            bp_inst,
            &me::bridge_port(G1612_BRIDGE_PROFILE, 1, 11, G1612_VEIP_ENDPOINT),
            &[1, 2, 3, 4],
        )?;
        println!("ZTE_G1612_VEIP_BP_CREATED instance={bp_inst:04x}");
    } else {
        println!("ZTE_G1612_VEIP_BP_REUSED instance={bp_inst:04x}");
    }

    // Check or create EVTO (ME 171 instance 0x0201)
    let evto_inst = G1612_VEIP_EVTO;
    if session.get(171, evto_inst, 0x8000).is_err() {
        create_verified_lenient(
            &mut session,
            171,
            evto_inst,
            &me::evto(10, G1612_VEIP_ENDPOINT),
            &[1, 7],
        )?;
        println!("ZTE_G1612_EVTO_CREATED instance={evto_inst:04x}");
    } else {
        println!("ZTE_G1612_EVTO_REUSED instance={evto_inst:04x}");
    }

    // Downstream mode transparent
    match session.set_attributes(171, evto_inst, &[(5, V::number(1))]) {
        Ok(()) => {
            let _ = verify(&mut session, 171, evto_inst, 0x0800, &[1]);
        }
        Err(error) => {
            println!("ZTE_G1612_EVTO_DOWNSTREAM_MODE_SKIPPED instance={evto_inst:04x}: {error}");
        }
    }

    if let Err(error) = apply_transparent_evto_table(&mut session, evto_inst) {
        println!("ZTE_G1612_EVTO_TABLE_SKIPPED instance={evto_inst:04x}: {error}");
    }

    // Bootstrap TR-069 ME 340 management server
    println!("ZTE_G1612_TR069_BOOTSTRAP id={id} instance={G1612_VEIP_ENDPOINT:04x}");
    crate::controller::management::bootstrap(&mut session, id, G1612_VEIP_ENDPOINT);

    verify(&mut session, 256, 0, 0x8000, &vendor_bytes)?;
    println!("ZTE_G1612_BRIDGE_SERVICE_READBACK_OK gem={port:04x} veip={G1612_VEIP_ENDPOINT:04x}");
    lease.disarm()?;
    println!("ZTE_G1612_LEASE_PERSISTED id={id}");
    Ok(())
}

fn retire_stale_ani(platform: PlatformContext, session: &mut OmciSession, current_port: u16) {
    for old in platform
        .resources()
        .allocations
        .iter()
        .map(|entry| entry.gem_id)
    {
        if old == current_port {
            continue;
        }
        let _ = session.delete(47, old);
        let _ = session.delete(266, old);
        let _ = session.delete(268, old);
        let _ = session.delete(84, old);
    }
}

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
        println!("ZTE_G1612_EVTO_TRANSPARENT_OK instance={instance:04x}");
    } else {
        println!("ZTE_G1612_EXISTING_EVTO_PRESERVED instance={instance:04x}");
    }
    Ok(())
}
