//! Realtek TYFCA1 profile and transparent bridge provisioning.
use crate::controller::business as me;
use crate::controller::business::{assign_tcont, create_verified_lenient};
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::attributes::AttributeValue as V;
use crate::pon_engine::omci::OmciSession;
use crate::pon_engine::ploam::PloamFrame;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

pub const TYFCA1_TCONT: u16 = 0x8000;
pub const TYFCA1_BRIDGE_PROFILE: u16 = 1;
pub const TYFCA1_GAL_PROFILE: u16 = 1;
pub const TYFCA1_VEIP_ENDPOINT: u16 = 0x0601;
pub const TYFCA1_VEIP_BP: u16 = 0x0201;
pub const TYFCA1_LAN_PORTS: [u16; 2] = [0x0101, 0x0102];

pub(super) fn configure(
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

    println!("REALTEK_TYFCA1_START id={id} port={port:04x}");

    // Send Assign_Alloc-ID downstream PLOAM (0x0A)
    let mut p = [0u8; 10];
    p[0] = (alloc >> 4) as u8;
    p[1] = (alloc << 4) as u8;
    p[2] = 1;
    for _ in 0..3 {
        let _ = platform.send_ploam(&PloamFrame::new(id, 0x0a, p));
    }
    std::thread::sleep(Duration::from_millis(50));

    // Ensure baseline OMCC and business grant floors
    lease.set_management_grant_floor(60)?;
    lease.set_business_grant_floor(1300)?;

    // Retire stale GEM/ANI entities and bridge ports from previous configurations
    let _ = session.delete(47, port);
    let _ = session.delete(266, port);
    let _ = session.delete(268, port);
    let _ = session.delete(84, port);
    for bp in [0x0201, 0x0202, 0x0203, 0x0601] {
        let _ = session.delete(171, bp);
        let _ = session.delete(47, bp);
    }

    // 1. Assign business Alloc-ID to T-CONT 0x8000
    println!("REALTEK_TYFCA1_ASSIGN_TCONT id={id} tcont={TYFCA1_TCONT:04x} alloc={alloc:04x}");
    assign_tcont(&mut session, TYFCA1_TCONT, alloc)?;

    // 2. GAL Ethernet profile (ME 272)
    let _ = create_verified_lenient(&mut session, 272, TYFCA1_GAL_PROFILE, &me::gal(), &[1]);

    // 3. MAC Bridge Service Profile (ME 45)
    let _ = create_verified_lenient(
        &mut session,
        45,
        TYFCA1_BRIDGE_PROFILE,
        &me::bridge_profile(),
        &[1, 2, 3],
    );

    // 4. GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    println!("REALTEK_TYFCA1_CREATE_GEM id={id} gem={port:04x}");
    create_verified_lenient(
        &mut session,
        268,
        port,
        &me::gem_ctp(port, TYFCA1_TCONT, TYFCA1_TCONT),
        &[1, 2, 3],
    )?;
    create_verified_lenient(
        &mut session,
        266,
        port,
        &me::gem_iw(port, TYFCA1_BRIDGE_PROFILE, TYFCA1_GAL_PROFILE),
        &[1, 2, 3, 4, 7],
    )?;

    // 5. Uplink ANI Bridge Port (ME 47, tp_type=5 GEM IW TP, port_num=6)
    let ani_port_num = 6u8;
    create_verified_lenient(
        &mut session,
        47,
        port,
        &me::bridge_port(TYFCA1_BRIDGE_PROFILE, ani_port_num, 5, port),
        &[1, 2, 3, 4],
    )?;

    // 6. VEIP Interface (ME 329 instance 0x0601)
    // Unlock VEIP: AdministrativeState = 0
    println!("REALTEK_TYFCA1_UNLOCK_VEIP id={id} instance={TYFCA1_VEIP_ENDPOINT:04x}");
    match session.set_attributes(329, TYFCA1_VEIP_ENDPOINT, &[(1, V::number(0))]) {
        Ok(()) => println!("REALTEK_TYFCA1_VEIP_UNLOCKED instance={TYFCA1_VEIP_ENDPOINT:04x}"),
        Err(e) => {
            println!("REALTEK_TYFCA1_VEIP_UNLOCK_FAILED instance={TYFCA1_VEIP_ENDPOINT:04x}: {e}")
        }
    }

    // Create VEIP Bridge Port (ME 47, tp_type=11 VEIP, port_num=1)
    let veip_port_num = 1u8;
    let veip_bp_inst = TYFCA1_VEIP_BP;
    let _ = session.delete(171, veip_bp_inst);
    let _ = session.delete(47, veip_bp_inst);

    create_verified_lenient(
        &mut session,
        47,
        veip_bp_inst,
        &me::bridge_port(
            TYFCA1_BRIDGE_PROFILE,
            veip_port_num,
            11,
            TYFCA1_VEIP_ENDPOINT,
        ),
        &[1, 2, 3, 4],
    )?;

    // Create VEIP EVTO (ME 171, association_type=10 VEIP)
    create_verified_lenient(
        &mut session,
        171,
        veip_bp_inst,
        &me::evto(10, TYFCA1_VEIP_ENDPOINT),
        &[1, 7],
    )?;

    // Downstream mode transparent (1)
    let _ = session.set_attributes(171, veip_bp_inst, &[(5, V::number(1))]);

    // Apply transparent tagging rule table
    if let Err(e) = apply_transparent_evto_table(&mut session, veip_bp_inst) {
        println!("REALTEK_TYFCA1_VEIP_EVTO_TABLE_SKIPPED instance={veip_bp_inst:04x}: {e}");
    }

    // Bootstrap TR-069
    println!("REALTEK_TYFCA1_TR069_BOOTSTRAP id={id} instance={TYFCA1_VEIP_ENDPOINT:04x}");
    crate::controller::management::bootstrap(&mut session, id, TYFCA1_VEIP_ENDPOINT);

    // 7. Downlink PPTP Ethernet UNI Bridge Ports & EVTOs
    for (idx, &endpoint) in TYFCA1_LAN_PORTS.iter().enumerate() {
        let uni_port_num = (idx as u8) + 2; // 2 for LAN1, 3 for LAN2
        let bp_inst = 0x0200 + u16::from(uni_port_num);

        // Unlock physical LAN UNI
        let _ = session.set_attributes(11, endpoint, &[(1, V::number(0))]);

        // Delete stale bridge port or EVTO if already exist on this instance
        let _ = session.delete(171, bp_inst);
        let _ = session.delete(47, bp_inst);

        // Create UNI Bridge Port (ME 47, tp_type=1 PPTP UNI)
        create_verified_lenient(
            &mut session,
            47,
            bp_inst,
            &me::bridge_port(TYFCA1_BRIDGE_PROFILE, uni_port_num, 1, endpoint),
            &[1, 2, 3, 4],
        )?;

        // Create EVTO (ME 171, association_type=2 PPTP Ethernet UNI)
        create_verified_lenient(&mut session, 171, bp_inst, &me::evto(2, endpoint), &[1, 7])?;

        // Downstream mode transparent (1)
        let _ = session.set_attributes(171, bp_inst, &[(5, V::number(1))]);

        // Apply transparent tagging rule table
        if let Err(e) = apply_transparent_evto_table(&mut session, bp_inst) {
            println!("REALTEK_TYFCA1_EVTO_TABLE_SKIPPED instance={bp_inst:04x}: {e}");
        }

        println!("REALTEK_TYFCA1_UNI_PORT_READY id={id} port={uni_port_num} endpoint={endpoint:04x} bp={bp_inst:04x}");
    }

    lease.disarm()?;
    println!("REALTEK_TYFCA1_PROVISION_OK id={id} port={port:04x} business=1300 management=60");
    Ok(())
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
        let readback = session.get_table(171, instance, 0x0400)?;
        if readback != expected {
            println!(
                "REALTEK_TYFCA1_EVTO_READBACK_DIFFERS instance={instance:04x} expected_len={} actual_len={}",
                expected.len(),
                readback.len()
            );
        } else {
            println!("REALTEK_TYFCA1_EVTO_TRANSPARENT_OK instance={instance:04x}");
        }
    } else {
        println!("REALTEK_TYFCA1_EXISTING_EVTO_PRESERVED instance={instance:04x}");
    }
    Ok(())
}
