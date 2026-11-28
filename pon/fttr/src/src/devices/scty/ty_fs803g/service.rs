//! COMHEART TY-FS803G (SCTY, Broadcom BCM68252) specific OMCI transparent bridge profile.
//!
//! Direct OMCI provisioning bypasses the 309-entity MIB upload which takes minutes
//! on Broadcom CMS protocol stacks and times out.
//!
//! Firmware analysis shows `rutGpon_isAllocIdInitValue` accepts 0xffff in all
//! application modes, whereas 0x00ff is initial only in mode 0. Reset to the
//! standard unassigned value before binding the business allocation. This
//! transition and successful OMCI readback do not prove RDPA or GEM forwarding;
//! those require independent live data-plane evidence.

use crate::controller::business as me;
use crate::controller::business::{create_verified_lenient, verify};
use crate::platform::{context::PlatformContext, openwrt::onu_lock::lock_onu};
use crate::pon_engine::omci::attributes::AttributeValue as V;
use crate::pon_engine::omci::OmciSession;
use crate::pon_engine::ploam::PloamFrame;
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

pub const TY_FS803G_TCONT: u16 = 0x8000;
pub const TY_FS803G_PRIO_QUEUE: u16 = 0x8000;
pub const TY_FS803G_VEIP_ENDPOINT: u16 = 0x0401;
pub const TY_FS803G_VEIP_BP: u16 = 0x0201;
pub const TY_FS803G_VEIP_EVTO: u16 = 0x0201;
pub const TY_FS803G_BRIDGE_PROFILE: u16 = 1;
pub const TY_FS803G_GAL_PROFILE: u16 = 1;

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

    println!("SCTY_TY_FS803G_PROVISION_START id={id} port={port:04x}");

    // Advertise the platform's business Alloc-ID with downstream PLOAM (0x0A).
    let mut p = [0u8; 10];
    p[0] = (alloc >> 4) as u8;
    p[1] = (alloc << 4) as u8;
    p[2] = 1;
    for _ in 0..3 {
        let _ = platform.send_ploam(&PloamFrame::new(id, 0x0a, p));
    }
    std::thread::sleep(Duration::from_millis(50));

    // Verify vendor
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN").to_vec();
    println!(
        "SCTY_TY_FS803G_VENDOR id={id} vendor={:?}",
        String::from_utf8_lossy(&vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported
    let _ = session.set_attributes(256, 0, &[(12, V::number(1))]);

    // Retire stale ANI/GEM entries from other slots
    retire_stale_ani(platform, &mut session, port)?;

    // Clean up any stale current_port entities before T-CONT transition
    let _ = session.delete(47, port);
    let _ = session.delete(266, port);
    let _ = session.delete(268, port);
    let _ = session.delete(84, port);

    // Use the mode-independent unassigned value, not the mode-0-only 0x00ff.
    let current_alloc = session
        .get(262, TY_FS803G_TCONT, 0x8000)
        .ok()
        .and_then(|frame| {
            frame
                .payload
                .get(3..5)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        });

    println!("SCTY_TY_FS803G_TCONT_CURRENT instance={TY_FS803G_TCONT:04x} alloc={current_alloc:?}");

    if current_alloc != Some(0xffff) {
        println!("SCTY_TY_FS803G_TCONT_RESET_UNASSIGNED instance={TY_FS803G_TCONT:04x}");
        session.set_attributes(262, TY_FS803G_TCONT, &[(1, V::number(0xffff))])?;
        verify(
            &mut session,
            262,
            TY_FS803G_TCONT,
            0x8000,
            &0xffffu16.to_be_bytes(),
        )?;
        std::thread::sleep(Duration::from_millis(50));
    }

    println!("SCTY_TY_FS803G_TCONT_SET_TARGET instance={TY_FS803G_TCONT:04x} alloc={alloc:04x}");
    session.set_attributes(262, TY_FS803G_TCONT, &[(1, V::number(alloc.into()))])?;
    std::thread::sleep(Duration::from_millis(50));
    verify(
        &mut session,
        262,
        TY_FS803G_TCONT,
        0x8000,
        &alloc.to_be_bytes(),
    )?;

    // Preserve the factory queue scheduler association. This firmware accepts
    // a null pointer in its queue setup path, but rejects changing 0000 to 8000
    // once the queue is in use (rcl_priorityQueueGObject, ret=9002). Neither a
    // matching scheduler entity number nor SET success proves it is writable.

    // GAL Ethernet profile (ME 272)
    if let Err(error) =
        create_verified_lenient(&mut session, 272, TY_FS803G_GAL_PROFILE, &me::gal(), &[1])
    {
        println!("SCTY_TY_FS803G_GAL_REUSED instance={TY_FS803G_GAL_PROFILE:04x}: {error}");
    }

    // MAC Bridge Service Profile (ME 45)
    if let Err(error) = create_verified_lenient(
        &mut session,
        45,
        TY_FS803G_BRIDGE_PROFILE,
        &me::bridge_profile(),
        &[1, 2, 3],
    ) {
        println!(
            "SCTY_TY_FS803G_BRIDGE_PROFILE_REUSED instance={TY_FS803G_BRIDGE_PROFILE:04x}: {error}"
        );
    }

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem_port(
        &mut session,
        port,
        port,
        TY_FS803G_TCONT,
        TY_FS803G_PRIO_QUEUE,
        TY_FS803G_BRIDGE_PROFILE,
        TY_FS803G_GAL_PROFILE,
    )?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP, port_num=6)
    let ani_port_num = 6u8;
    create_verified_lenient(
        &mut session,
        47,
        port,
        &me::bridge_port(TY_FS803G_BRIDGE_PROFILE, ani_port_num, 5, port),
        &[1, 2, 3, 4],
    )?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified_lenient(&mut session, 84, port, &me::vlan_filter(), &[1, 2]);

    // Check or create VEIP Bridge Port (ME 47 instance 0x0201)
    let bp_inst = TY_FS803G_VEIP_BP;
    let bp_present = match session.get(47, bp_inst, 0xf000) {
        Ok(frame) => veip_bridge_present(&frame.payload[3..9])?,
        Err(error) if crate::pon_engine::omci::error::is_absent(&error) => false,
        Err(error) => return Err(error),
    };
    if !bp_present {
        create_verified_lenient(
            &mut session,
            47,
            bp_inst,
            &me::bridge_port(TY_FS803G_BRIDGE_PROFILE, 1, 11, TY_FS803G_VEIP_ENDPOINT),
            &[1, 2, 3, 4],
        )?;
        println!("SCTY_TY_FS803G_VEIP_BP_CREATED instance={bp_inst:04x}");
    } else {
        println!("SCTY_TY_FS803G_VEIP_BP_REUSED instance={bp_inst:04x}");
    }

    // Check or create EVTO (ME 171 instance 0x0201)
    let evto_inst = TY_FS803G_VEIP_EVTO;
    let evto_present = match session.get(171, evto_inst, 0x8200) {
        Ok(frame) => veip_evto_present(&frame.payload[3..6])?,
        Err(error) if crate::pon_engine::omci::error::is_absent(&error) => false,
        Err(error) => return Err(error),
    };
    if !evto_present {
        create_verified_lenient(
            &mut session,
            171,
            evto_inst,
            &me::evto(10, TY_FS803G_VEIP_ENDPOINT),
            &[1, 7],
        )?;
        println!("SCTY_TY_FS803G_EVTO_CREATED instance={evto_inst:04x}");
    } else {
        println!("SCTY_TY_FS803G_EVTO_REUSED instance={evto_inst:04x}");
    }

    // Downstream mode transparent
    match session.set_attributes(171, evto_inst, &[(5, V::number(1))]) {
        Ok(()) => {
            let _ = verify(&mut session, 171, evto_inst, 0x0800, &[1]);
        }
        Err(error) => {
            println!(
                "SCTY_TY_FS803G_EVTO_DOWNSTREAM_MODE_SKIPPED instance={evto_inst:04x}: {error}"
            );
        }
    }

    if let Err(error) = session.with_timeout(Duration::from_secs(30), |session| {
        apply_transparent_evto_table(session, evto_inst)
    }) {
        if matches!(
            error.kind(),
            io::ErrorKind::NotConnected | io::ErrorKind::Interrupted
        ) {
            return Err(error);
        }
        println!("SCTY_TY_FS803G_EVTO_TABLE_SKIPPED instance={evto_inst:04x}: {error}");
    }

    // Ensure VEIP ME 329 administrative state is unlocked (0) to trigger Broadcom rutRgFull_configVeip
    if let Err(error) = session.set_attributes(329, TY_FS803G_VEIP_ENDPOINT, &[(1, V::number(0))]) {
        println!("SCTY_TY_FS803G_VEIP_UNLOCK_SKIPPED: {error}");
    }

    // Unlock LAN Ethernet UNIs (ME 11 instances 0xff03, 0xff04)
    for uni in [0xff03u16, 0xff04] {
        let _ = session.set_attributes(11, uni, &[(5, V::number(0))]);
    }

    // Bootstrap TR-069 ME 340 management server
    println!("SCTY_TY_FS803G_TR069_BOOTSTRAP id={id} instance={TY_FS803G_VEIP_ENDPOINT:04x}");
    crate::controller::management::bootstrap(&mut session, id, TY_FS803G_VEIP_ENDPOINT);

    // Current miniOLT DBRu-driven scheduling leaves this model with report-only
    // grants. A bounded measured floor permits upstream traffic while leaving
    // most shared upstream bandwidth available to other ONUs. Keep this policy
    // model-local; the platform lease preserves it across temporary diagnostics.
    lease.set_business_grant_floor(1300)?;
    // The measured two-byte OMCC polling allocation causes receive errors when
    // followed by this ONU's business allocation. A 60-byte allocation preserved
    // both IDs and eliminated BIP growth in bounded field comparisons. Persist
    // it through the same reversible lease, and require the final OMCI GET below
    // to succeed before committing ownership. This is not a proof of IP service.
    lease.set_management_grant_floor(60)?;
    println!("SCTY_TY_FS803G_BUSINESS_GRANT_FLOOR id={id} bytes=1300");

    verify(&mut session, 256, 0, 0x8000, &vendor_bytes)?;
    println!(
        "SCTY_TY_FS803G_BRIDGE_SERVICE_READBACK_OK gem={port:04x} veip={TY_FS803G_VEIP_ENDPOINT:04x}"
    );

    // Keep Alloc-ID and FPGA channel permanently armed for business
    lease.disarm()?;
    println!("SCTY_TY_FS803G_LEASE_PERSISTED id={id} port={port:04x}");
    Ok(())
}

fn veip_bridge_present(data: &[u8]) -> io::Result<bool> {
    // An entirely empty factory record has no ownership to preserve. Never
    // replace a nonempty foreign binding, including a partially initialized one.
    if data == [0; 6] {
        return Ok(false);
    }
    if data.len() == 6
        && data[..2] == TY_FS803G_BRIDGE_PROFILE.to_be_bytes()
        && data[3] == 11
        && data[4..6] == TY_FS803G_VEIP_ENDPOINT.to_be_bytes()
    {
        return Ok(true);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "VEIP bridge port association mismatch",
    ))
}

fn veip_evto_present(data: &[u8]) -> io::Result<bool> {
    match data {
        [0, 0, 0] => Ok(false),
        [10, 0x04, 0x01] => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VEIP EVTO association mismatch",
        )),
    }
}

fn retire_stale_ani(
    platform: PlatformContext,
    session: &mut OmciSession,
    current_port: u16,
) -> io::Result<()> {
    for old in platform
        .resources()
        .allocations
        .iter()
        .map(|entry| entry.gem_id)
    {
        if old == current_port {
            continue;
        }
        // A successful GET alone does not prove ownership. Some firmware returns
        // zero/default values for unused instances. Only retire our bridge's ANI
        // binding to the corresponding slot GEM, not arbitrary existing entities.
        let frame = match session.get(47, old, 0xf000) {
            Ok(frame) => frame,
            Err(error) if crate::pon_engine::omci::error::is_absent(&error) => continue,
            Err(error) => return Err(error),
        };
        let binding = &frame.payload[3..9];
        if stale_ani_binding(binding, old) {
            println!("SCTY_TY_FS803G_RETIRE_STALE_ANI instance={old:04x}");
            for class in [47, 266, 268, 84] {
                match session.delete(class, old) {
                    Ok(()) => println!("SCTY_TY_FS803G_RETIRED class={class} instance={old:04x}"),
                    Err(error) if crate::pon_engine::omci::error::is_absent(&error) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(())
}

fn stale_ani_binding(binding: &[u8], gem: u16) -> bool {
    binding.len() == 6
        && binding[..2] == TY_FS803G_BRIDGE_PROFILE.to_be_bytes()
        && binding[3] == 5
        && binding[4..6] == gem.to_be_bytes()
}

fn create_gem_port(
    session: &mut OmciSession,
    instance: u16,
    port: u16,
    tcont: u16,
    prio_queue: u16,
    bridge: u16,
    gal: u16,
) -> io::Result<()> {
    create_verified_lenient(
        session,
        268,
        instance,
        &me::gem_ctp(port, tcont, prio_queue),
        &[1, 2, 3],
    )?;
    match session.get(268, instance, 0x1000) {
        Ok(frame) if frame.payload[3..5] != prio_queue.to_be_bytes() => {
            session.set_attributes(268, instance, &[(4, V::number(prio_queue.into()))])?;
            println!(
                "GEM_UPSTREAM_TM_REPAIRED instance={instance:04x} prio_queue={prio_queue:04x}"
            );
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
    println!(
        "SCTY_TY_FS803G_EVTO_TABLE instance={instance:04x} len={}",
        before.len()
    );
    let rules = transparent_rules();
    validate_evto_table(&before)?;
    // Preserve unrelated factory rules and avoid rewriting already correct rows.
    for (index, row) in rules.iter().enumerate() {
        if before.chunks_exact(16).any(|existing| existing == row) {
            continue;
        }
        session
            .set_attributes(171, instance, &[(6, V::octets(row))])
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("EVTO SET instance={instance:04x} row={index}: {error}"),
                )
            })?;
        println!("SCTY_TY_FS803G_EVTO_ROW_SET instance={instance:04x} row={index}");
    }
    let after = session.get_table(171, instance, 0x0400)?;
    validate_evto_table(&after)?;
    if !rules
        .iter()
        .all(|row| after.chunks_exact(16).any(|existing| existing == row))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("EVTO readback missing transparent rules instance={instance:04x}"),
        ));
    }
    println!(
        "SCTY_TY_FS803G_EVTO_TRANSPARENT_READY instance={instance:04x} len={}",
        after.len()
    );
    Ok(())
}

fn validate_evto_table(table: &[u8]) -> io::Result<()> {
    if table.len() % 16 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "EVTO table has incomplete row",
        ));
    }
    Ok(())
}

fn transparent_rules() -> Vec<Vec<u8>> {
    [
        (0xf8000000u32, 0xf8000000u32),
        (0xf8000000, 0xe8000000),
        (0xe8000000, 0xe8000000),
    ]
    .into_iter()
    .map(|(outer, inner)| {
        [outer, inner, 0x000f0000, 0x000f0000]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect()
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
    use std::collections::VecDeque;

    // Script complete GET/GET NEXT exchanges, including ONU-side SET rejection
    // and accepted writes that fail to appear in a subsequent table snapshot.
    struct Script {
        replies: VecDeque<(u8, [u8; 32])>,
        pending: Option<BaselineFrame>,
    }
    impl OmciTransport for Script {
        fn send(&mut self, request: &BaselineFrame) -> io::Result<()> {
            let (kind, payload) = self.replies.pop_front().expect("unexpected OMCI operation");
            assert_eq!(request.message_type, kind);
            self.pending = Some(BaselineFrame::new(
                request.transaction_id,
                kind & !0x40 | 0x20,
                request.class_id,
                request.instance_id,
                payload,
            ));
            Ok(())
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            Ok(self.pending.take())
        }
    }
    fn table_replies(table: &[u8]) -> Vec<(u8, [u8; 32])> {
        let mut payload = [0; 32];
        payload[1..3].copy_from_slice(&0x0400u16.to_be_bytes());
        payload[3..7].copy_from_slice(&(table.len() as u32).to_be_bytes());
        let mut replies = vec![(0x49, payload)];
        for chunk in table.chunks(29) {
            let mut payload = [0; 32];
            payload[1..3].copy_from_slice(&0x0400u16.to_be_bytes());
            payload[3..3 + chunk.len()].copy_from_slice(chunk);
            replies.push((0x5a, payload));
        }
        replies
    }
    fn session(replies: Vec<(u8, [u8; 32])>) -> OmciSession {
        OmciSession::new(Script {
            replies: replies.into(),
            pending: None,
        })
    }
    #[test]
    fn complete_factory_rules_are_reused_without_set() {
        let table = transparent_rules().concat();
        let mut replies = table_replies(&table);
        replies.extend(table_replies(&table));
        apply_transparent_evto_table(&mut session(replies), 0x0201).unwrap();
    }
    #[test]
    fn rejected_write_stops_before_reporting_ready() {
        let mut replies = table_replies(&[]);
        let mut rejected = [0; 32];
        rejected[0] = 3;
        replies.push((0x48, rejected));
        let error = apply_transparent_evto_table(&mut session(replies), 0x0201).unwrap_err();
        assert!(error.to_string().contains("row=0"));
        assert!(error.to_string().contains("result=3"));
    }
    #[test]
    fn accepted_writes_require_matching_readback() {
        let mut replies = table_replies(&[]);
        replies.extend([(0x48, [0; 32]); 3]);
        replies.extend(table_replies(&[]));
        let error = apply_transparent_evto_table(&mut session(replies), 0x0201).unwrap_err();
        assert!(error.to_string().contains("missing transparent rules"));
    }
    #[test]
    fn factory_recovery_never_authorizes_replacing_a_foreign_veip_binding() {
        assert!(!veip_bridge_present(&[0; 6]).unwrap());
        assert!(!veip_evto_present(&[0; 3]).unwrap());
        assert!(veip_bridge_present(&[0, 1, 1, 11, 4, 1]).unwrap());
        // Reuse the actual port number rather than overwriting a valid endpoint.
        assert!(veip_bridge_present(&[0, 1, 7, 11, 4, 1]).unwrap());
        assert!(veip_evto_present(&[10, 4, 1]).unwrap());
        for foreign in [[0, 2, 1, 11, 4, 1], [0, 1, 1, 11, 4, 2], [0, 0, 1, 0, 0, 0]] {
            assert!(veip_bridge_present(&foreign).is_err());
        }
        assert!(veip_bridge_present(&[]).is_err());
        assert!(veip_evto_present(&[10, 4, 2]).is_err());
        assert!(veip_evto_present(&[0, 4, 1]).is_err());
    }
    #[test]
    fn stale_cleanup_requires_owned_ani_binding() {
        assert!(stale_ani_binding(&[0, 1, 6, 5, 1, 3], 0x0103));
        assert!(!stale_ani_binding(&[0; 6], 0x0103));
        assert!(!stale_ani_binding(&[0, 2, 6, 5, 1, 3], 0x0103));
        assert!(!stale_ani_binding(&[0, 1, 1, 11, 4, 1], 0x0103));
        assert!(!stale_ani_binding(&[0, 1, 6, 5, 1, 4], 0x0103));
    }
}
