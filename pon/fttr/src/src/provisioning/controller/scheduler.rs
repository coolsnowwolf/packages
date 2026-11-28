//! One bounded reconciliation transaction per scheduler tick.
use super::*;
use crate::provisioning::{bootstrap, link_identity, management_network, storage};

pub(super) fn tick(engine: &Arc<Mutex<crate::pon_engine::PonEngine>>, batch: Ready) -> Duration {
    // Release the optical lock before consulting ACS or the controller.
    let online_onus: BTreeMap<_, _> = engine
        .lock()
        .unwrap()
        .active_onus
        .iter()
        .filter(|o| o.state == crate::pon_engine::ploam::OnuState::O5Operation)
        .map(|o| (u16::from(o.onu_id), o.full_sn_str()))
        .collect();
    let online: BTreeSet<_> = online_onus.values().cloned().collect();
    let resources = engine.lock().unwrap().resources;
    let interfaces = resources
        .onu_ids
        .iter()
        .filter_map(|&id| {
            resources
                .interface(id)
                .map(|name| (u16::from(id), name.to_owned()))
        })
        .collect();
    let source_links = link_identity::discover(&online_onus, &interfaces);
    if let Ok(mut registry) = crate::mqtt_identity::shared().lock() {
        registry.update_topology(online_onus.clone(), source_links.clone(), now());
    }
    #[cfg(feature = "acs")]
    let (devices, acs_status) = crate::acs::observations();
    #[cfg(not(feature = "acs"))]
    let (devices, acs_status) = (
        DeviceObservations::default(),
        ListenerObservation::default(),
    );
    controller().lock().unwrap().activate_pending_config();
    let configured = controller().lock().unwrap().config.clone();
    crate::controller::mqtt_bridge::tick(&online_onus, &interfaces, &configured.devices);
    // Bootstrap-free ACS consumers must also reach the new listener before the
    // old network is retired. They have no bootstrap record to satisfy that gate.
    let cwmp_business_ready =
        bootstrap_free_consumers_ready(&configured, &devices, &acs_status, &online, now());
    management_network::tick(configured.management_network.as_ref(), cwmp_business_ready);
    let active_management = management_network::active();
    crate::controller::acs_bridge::tick(
        &online_onus,
        &interfaces,
        &configured.devices,
        configured.management_network.as_ref(),
        active_management.is_some(),
        &devices,
        source_links.as_ref().ok(),
    );
    let mut c = controller().lock().unwrap();
    c.acs_status = acs_status;
    c.resolve_pending_identities(&devices);
    let config = c.config.clone();
    bootstrap::retain_devices(
        &config
            .devices
            .iter()
            .filter(|d| d.bootstrap_adapter != "none" && !c.resets.blocks(&d.optical_sn))
            .map(|d| d.id.clone())
            .collect(),
    );
    let time = now();
    c.optical_epochs.retain(|serial, _| {
        online.contains(serial) && config.devices.iter().any(|d| &d.optical_sn == serial)
    });
    for (id, serial) in &online_onus {
        if !config.devices.iter().any(|d| &d.optical_sn == serial) {
            continue;
        }
        if c.optical_epochs
            .get(serial)
            .is_none_or(|(previous, _)| previous != id)
        {
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            c.optical_epochs.insert(serial.clone(), (*id, epoch));
        }
    }
    let mut fresh = BTreeSet::new();
    let mut command_ready = BTreeSet::new();
    c.begin_history_tick();
    c.collect_commands(time);
    bootstrap::collect();
    // Retry a failed barrier before considering any dependent work. A corrupt
    // load remains blocked; observations must never overwrite that evidence.
    if !c.persist_snapshots() {
        return Duration::from_secs(10);
    }
    let mut history_changed = false;
    for d in &config.devices {
        if !crate::omci_auth::allowed(&d.optical_sn) {
            continue;
        }
        if c.resets.blocks(&d.optical_sn) {
            continue;
        }
        // Observe both channels and advance bootstrap independently of business transport.
        let observed = devices.get(&d.cwmp_id);
        let at = observed.and_then(|value| c.acs_status.current_inform(value));
        let cwmp_reachable = online.contains(&d.optical_sn)
            && at.is_some_and(|at| {
                at <= time
                    && time - at <= d.connection_request.effective_freshness(config.freshness)
            });
        let state = if !d.cwmp_enabled() {
            "disabled"
        } else if cwmp_reachable {
            "online"
        } else if at.is_none() {
            "waiting_inform"
        } else {
            "offline_or_stale"
        };
        c.management.insert(d.id.clone(), json!({"cwmp_id":d.cwmp_id,"product_class":observed.map(|d| &d.product_class),"state":state,"optical_online":online.contains(&d.optical_sn),"last_inform":at,"age_seconds":at.map(|at| time.saturating_sub(at))}));
        let mqtt_reachable = online.contains(&d.optical_sn)
            && crate::mqtt_identity::shared()
                .lock()
                .unwrap()
                .automatic_source(&d.optical_sn, time)
                .ok()
                .flatten()
                .is_some();
        c.mqtt.insert(d.id.clone(), json!({"state":if mqtt_reachable {"online"} else {"waiting_transport"},"optical_online":online.contains(&d.optical_sn)}));
        let boot = observed.is_some_and(|device| device.boot);
        let new_boot = boot && at.is_some() && at != c.last_boot.get(&d.id).copied();
        if boot {
            if let Some(at) = at {
                c.last_boot.insert(d.id.clone(), at);
            }
        }
        if d.bootstrap_adapter != "none" {
            bootstrap::tick(bootstrap::Observation {
                device: d,
                management: active_management.as_ref(),
                last_inform: at,
                online: online.contains(&d.optical_sn),
                new_boot,
                freshness: d.connection_request.effective_freshness(config.freshness),
                time,
                acs_available: active_management.is_some()
                    && c.acs_status.enabled
                    && c.acs_status.error.is_none(),
                authorization: management_network::grant(&d.id, &d.optical_sn),
            });
        }
        if d.backend_kind() == Backend::Disabled {
            continue;
        }
        if d.backend_kind() == Backend::Command {
            let reachable = mqtt_reachable;
            if reachable {
                fresh.insert(d.id.clone());
                command_ready.insert(d.id.clone());
            }
            schedule(&mut c, d, reachable, false, time);
            continue;
        }
        let host_matches = config
            .management_network
            .as_ref()
            .is_none_or(management_network::matches_desired);
        let reachable = host_matches && cwmp_reachable;
        if reachable {
            fresh.insert(d.id.clone());
        }
        schedule(&mut c, d, reachable, new_boot, time);
        if !c.persist_snapshots() {
            continue;
        }
        let capability_identity = identity(d, observed);
        use std::hash::{Hash, Hasher};
        let mut epoch_hash = std::collections::hash_map::DefaultHasher::new();
        (
            c.last_boot.get(&d.id).copied().unwrap_or(0),
            c.optical_epochs
                .get(&d.optical_sn)
                .map(|(_, epoch)| *epoch)
                .unwrap_or(0),
        )
            .hash(&mut epoch_hash);
        let capability_epoch = epoch_hash.finish();
        let availability_changed = reachable != c.last_online.contains(&d.id);
        let reload_pending = c.pending_config.is_some()
            || c.operations
                .get(&d.id)
                .is_some_and(|op| op.revision != config.device_revision(&d.id));
        if let Some(op) = c.operations.get_mut(&d.id) {
            if op.stage == OperationPhase::Completed
                && (new_boot
                    || op
                        .capabilities
                        .observation_changed(&capability_identity, capability_epoch))
            {
                op.capabilities.invalidate("identity, firmware, optical registration or BOOTSTRAP changed; evidence is stale");
                history_changed = true;
            }
            if op.device.cwmp_id.starts_with("pending:") && !d.cwmp_id.starts_with("pending:") {
                history_changed = true;
                op.device.cwmp_id = d.cwmp_id.clone();
                match (wan::load(&d.cwmp_id), wifi::load(&d.cwmp_id)) {
                    (Ok(owned), Ok(registry)) => {
                        let cleanup = op.wan.full_sync();
                        op.wan =
                            wan::Reconciler::new(&op.device, owned.unwrap_or_default(), cleanup);
                        op.wifi = wifi::Reconciler::new(&op.device, registry, cleanup);
                    }
                    _ => {
                        op.complete(now());
                        op.wan.results.insert(
                            "identity".into(),
                            json!({"state":"blocked","reason":"cannot load device ownership"}),
                        );
                    }
                }
            }
            if op.stage != OperationPhase::Completed
                && (batch.all
                    || batch.devices.contains(&d.cwmp_id)
                    || new_boot
                    || availability_changed)
            {
                if config.management_network.is_some() && !host_matches {
                    continue;
                }
                let before = (
                    op.stage,
                    op.index,
                    op.task,
                    op.compiled,
                    op.capabilities.progress(),
                    op.wan.progress(),
                    op.wifi.progress(),
                );
                op.work_started = true;
                let publish = reconcile(
                    op,
                    capability_identity,
                    capability_epoch,
                    reachable,
                    reload_pending,
                );
                let after = (
                    op.stage,
                    op.index,
                    op.task,
                    op.compiled,
                    op.capabilities.progress(),
                    op.wan.progress(),
                    op.wifi.progress(),
                );
                if before != after && op.stage != OperationPhase::Completed {
                    wake_device(&d.cwmp_id);
                }
                if !publish {
                    continue;
                }
            }
            if let Err(error) = op.wan.save(&op.device.cwmp_id) {
                history_changed = true;
                op.wan.results.insert(
                    "persistence".into(),
                    json!({"state":"blocked","reason":error}),
                );
            }
            if let Err(error) = op.wifi.save(&op.device.cwmp_id) {
                history_changed = true;
                op.wifi.results.insert(
                    "persistence".into(),
                    json!({"state":"blocked","reason":error}),
                );
                op.complete(now());
            }
            let wifi_owned = op.wifi.registry.clone();
            c.wifi_owned.insert(d.cwmp_id.clone(), wifi_owned);
            let owned = c.operations.get(&d.id).unwrap().wan.owned.clone();
            c.owned.insert(d.cwmp_id.clone(), owned);
        }
    }
    let durable = c.persist_snapshots();
    c.admit_commands(
        &if durable {
            command_ready
        } else {
            BTreeSet::new()
        },
        time,
        |op| {
            if op.recovers().is_some() {
                return crate::provisioning::command::Job::recover(
                    op.device.clone(),
                    op.apply,
                    op.id,
                );
            }
            if op.observes().is_some() {
                return crate::provisioning::command::Job::observe(
                    op.device.clone(),
                    op.command().and_then(|s| s.reboot_from.clone()),
                );
            }
            crate::provisioning::command::Job::start(
                op.device.clone(),
                op.apply,
                op.full_sync,
                op.revision.clone(),
                op.id,
            )
        },
    );
    c.compact_completed();
    c.last_online = fresh;
    c.dirty |= history_changed;
    c.persist_history(|data| {
        storage::write_atomic(
            crate::provisioning::runtime_store::LEGACY_HISTORY,
            data,
            false,
        )
    });
    if c.activate_pending_config() {
        // Do not wait for the periodic sweep after the final old operation finishes.
        wake();
        return Duration::ZERO;
    }
    // State remains authoritative; the low-frequency sweep also detects optical
    // changes from backends that do not publish CWMP events.
    let deadline = c
        .operations
        .values()
        .flat_map(|op| match &op.execution {
            Execution::Acs(state) => [state.verify_after, state.capabilities.deadline()],
            _ => [0, 0],
        })
        .chain(c.next_retry_at.values().copied())
        .filter(|at| *at > time)
        .min();
    Duration::from_secs(
        deadline
            .map_or(10, |at| at.saturating_sub(time).min(10))
            .max(1),
    )
}

fn bootstrap_free_consumers_ready(
    config: &Config,
    devices: &DeviceObservations,
    acs: &ListenerObservation,
    online: &BTreeSet<String>,
    time: u64,
) -> bool {
    config
        .devices
        .iter()
        .filter(|d| d.bootstrap_adapter == "none" && d.backend() == "acs")
        .all(|d| {
            let id = discovered_identity(devices, &d.optical_sn).unwrap_or(&d.cwmp_id);
            devices
                .get(id)
                .and_then(|observed| acs.current_inform(observed))
                .is_some_and(|at| {
                    online.contains(&d.optical_sn)
                        && at <= time
                        && time - at <= d.connection_request.effective_freshness(config.freshness)
                })
        })
}

#[cfg(test)]
#[test]
fn bootstrap_free_acs_consumers_require_current_listener_evidence_before_network_cleanup() {
    let mut device = tests::operation(OperationPhase::Completed, false).device;
    device.bootstrap_adapter = "none".into();
    let mut config = Config::default();
    config.devices.push(device.clone());
    let online = BTreeSet::from([device.optical_sn.clone()]);
    use crate::cwmp::observations::DeviceObservation;
    let mut devices = DeviceObservations::new();
    let observation = |endpoint: &str, last_inform| DeviceObservation {
        endpoint_id: endpoint.into(),
        last_inform,
        ..Default::default()
    };
    devices.insert(device.cwmp_id.clone(), observation("new-listener", 100));
    let acs = ListenerObservation {
        enabled: true,
        primary: "new-listener".into(),
        ..Default::default()
    };
    assert!(bootstrap_free_consumers_ready(
        &config, &devices, &acs, &online, 101
    ));
    for (endpoint, inform, time) in [
        ("old-listener", 100, 101),
        ("new-listener", 102, 101),
        ("new-listener", 0, 100000),
    ] {
        devices.insert(device.cwmp_id.clone(), observation(endpoint, inform));
        assert!(!bootstrap_free_consumers_ready(
            &config, &devices, &acs, &online, time
        ));
    }
    devices.insert(device.cwmp_id.clone(), observation("new-listener", 100));
    assert!(!bootstrap_free_consumers_ready(
        &config,
        &devices,
        &acs,
        &BTreeSet::new(),
        101
    ));
    for adapter in ["none", "hl202-mqtt"] {
        config.devices[0].provisioning_adapter = adapter.into();
        assert!(bootstrap_free_consumers_ready(
            &config,
            &Default::default(),
            &Default::default(),
            &BTreeSet::new(),
            101
        ));
    }

    // Extra LAN listener endpoint IDs are recognized even when primary endpoint_id is empty/different,
    // while retired/stale endpoints are still rejected.
    config.devices[0].provisioning_adapter = "hl202-cu".into();
    let extra_acs = ListenerObservation {
        enabled: true,
        extra: Some("extra-lan-listener".into()),
        ..Default::default()
    };
    devices.insert(
        device.cwmp_id.clone(),
        observation("extra-lan-listener", 100),
    );
    assert!(bootstrap_free_consumers_ready(
        &config, &devices, &extra_acs, &online, 101
    ));
    devices.insert(
        device.cwmp_id.clone(),
        observation("retired-extra-listener", 100),
    );
    assert!(!bootstrap_free_consumers_ready(
        &config, &devices, &extra_acs, &online, 101
    ));
}

#[derive(Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Reasons(pub(super) u8);
impl Reasons {
    fn add(&mut self, bit: u8, present: bool) {
        if present {
            self.0 |= bit;
        }
    }
}
fn retry_delay(attempts: u32) -> u64 {
    (15u64 * (1u64 << attempts.saturating_sub(1).min(5))).min(300)
}

fn schedule(c: &mut Controller, d: &Device, reachable: bool, new_boot: bool, time: u64) {
    if c.pending_config.is_some() {
        return;
    }
    if c.unresolved_owner(d) {
        c.automatic_ready.remove(&d.id);
        c.schedule_observation(d, reachable, time);
        return;
    }
    // A tmpfs checkpoint missing after reboot cannot establish the fate of old
    // remote writes. Discover afresh, but require explicit apply to rearm writes.
    let write_admitted =
        c.snapshots.is_none() || crate::provisioning::runtime_store::approved(&d.optical_sn);
    if !write_admitted {
        let previous = c.previous_facts(&d.id);
        if reachable && previous.is_none() && c.config.enabled && c.config.automatic {
            let _ = c.start_selected(&d.id, false, false, None);
            return;
        }
    }
    let previous = c.previous_facts(&d.id);
    let needs_reconcile = previous
        .as_ref()
        .is_some_and(|p| p.reconcile && !p.unresolved);
    let needs_manual = previous.as_ref().is_some_and(|p| {
        p.unresolved || (p.phase == Some(OperationPhase::Interrupted) && !p.reconcile)
    });
    let all_verified = previous
        .as_ref()
        .is_some_and(|p| p.verified && p.apply && p.revision == c.config.device_revision(&d.id));
    if all_verified {
        c.attempts.remove(&d.id);
        c.next_retry_at.remove(&d.id);
        // An explicit successful apply also satisfies causes queued before its
        // admission; do not immediately replace its result with a startup apply.
        c.automatic_ready.remove(&d.id);
        c.startup_pending.remove(&d.id);
    }
    let active_op = c.operations.get(&d.id).is_some_and(|o| {
        o.stage != OperationPhase::Completed && o.stage != OperationPhase::Interrupted
    });
    let startup_trigger = c.startup_trigger_pending(&d.id);
    let config_changed = c.config.triggers.contains("config_change")
        && previous
            .as_ref()
            .is_none_or(|p| p.revision != c.config.device_revision(&d.id));
    let periodic = c.periodic_due(&d.id, time);
    // Merge independent trigger authority into one bounded per-device ready set.
    // A read-only plan's drift never contributes write authority.
    let reasons = c.automatic_ready.entry(d.id.clone()).or_default();
    reasons.add(1, config_changed);
    reasons.add(2, startup_trigger);
    reasons.add(4, periodic);
    reasons.add(
        8,
        c.config.triggers.contains("device_online") && !c.last_online.contains(&d.id),
    );
    reasons.add(16, new_boot && c.config.triggers.contains("cwmp_boot"));
    reasons.add(32, previous.as_ref().is_some_and(|p| p.apply && p.drift));
    reasons.add(
        64,
        c.attempts.get(&d.id).is_some_and(|n| *n > 0)
            && previous.as_ref().is_some_and(|p| {
                p.apply && p.phase == Some(OperationPhase::Completed) && !p.verified
            }),
    );
    let reasons = *reasons;
    if needs_manual {
        c.automatic_ready.remove(&d.id);
        return;
    }
    let attempts = c.attempts.get(&d.id).copied().unwrap_or(0);
    // Backoff starts at terminal completion, not the start of a potentially
    // slow operation. Repeated observations cannot move this fixed anchor.
    if !active_op && attempts != 0 {
        if let Some(completed) = previous.as_ref().and_then(|p| p.completed_at) {
            let at = completed.saturating_add(retry_delay(attempts));
            let deadline = c.next_retry_at.entry(d.id.clone()).or_default();
            *deadline = (*deadline).max(at);
        }
    }
    if !active_op && reachable && c.config.enabled && c.config.automatic {
        // Recovery of a completed read-only failure has its own bounded episode;
        // it never grants write authority, even when periodic sync is disabled.
        if previous.as_ref().is_some_and(|p| {
            !p.apply
                && ((p.phase == Some(OperationPhase::Completed) && p.failed)
                    || (p.phase == Some(OperationPhase::Interrupted) && p.reconcile))
        }) {
            let episode = c.read_recovery.entry(d.id.clone()).or_default();
            if let Some(completed) = previous.as_ref().and_then(|p| p.completed_at) {
                episode.retry_at = episode
                    .retry_at
                    .max(completed.saturating_add(retry_delay(episode.attempts + 1)));
            } else if episode.retry_at == 0 {
                episode.retry_at = time.saturating_add(retry_delay(episode.attempts + 1));
            }
            let attempt = episode.attempts;
            if attempt < 3
                && time >= episode.retry_at
                && c.start_selected(&d.id, false, false, None).is_ok()
            {
                let episode = c.read_recovery.entry(d.id.clone()).or_default();
                episode.attempts += 1;
            }
            return;
        }
        if previous.as_ref().is_some_and(|p| {
            !p.apply && p.verified && p.revision == c.config.device_revision(&d.id)
        }) {
            c.read_recovery.remove(&d.id);
        }
        if needs_reconcile {
            let _ = c.start_selected(&d.id, false, false, None);
        } else if reasons.0 != 0
            && attempts < 5
            && time >= c.next_retry_at.get(&d.id).copied().unwrap_or(0)
        {
            if write_admitted && c.start(&d.id, true, false).is_ok() {
                let next_attempts = attempts + 1;
                c.attempts.insert(d.id.clone(), next_attempts);
                c.next_retry_at.insert(
                    d.id.clone(),
                    time.saturating_add(retry_delay(next_attempts)),
                );
                c.automatic_ready.remove(&d.id);
                if reasons.0 & 2 != 0 {
                    c.startup_pending.remove(&d.id);
                }
            }
        }
    }
}

#[cfg(test)]
mod config_change_tests {
    use super::*;

    #[test]
    #[cfg_attr(
        not(all(feature = "device-hl202", feature = "acs")),
        ignore = "requires HL202 ACS adapter"
    )]
    fn unrelated_template_edit_does_not_reschedule_a_verified_device() {
        let mut c = Controller::default();
        c.replace_config(super::super::lifecycle_tests::two_device_config(
            "slow-before",
            "fast-unchanged",
        ))
        .unwrap();
        let fast = c
            .config
            .devices
            .iter()
            .find(|d| d.id == "fast")
            .unwrap()
            .clone();
        crate::omci_auth::test_authorize(&fast.optical_sn);
        let revision = c.config.device_revision("fast").to_owned();
        let mut op = Operation::new(41, fast.clone(), revision.clone(), true, false);
        for item in &fast.items {
            op.results.insert(
                item.id.clone(),
                json!({"id":item.id,"state":"verified","revision":revision}),
            );
        }
        op.complete(100);
        c.operations.insert("fast".into(), op);
        c.request_config(super::super::lifecycle_tests::two_device_config(
            "slow-after",
            "fast-unchanged",
        ));
        assert_eq!(c.config.device_revision("fast"), revision);
        schedule(&mut c, &fast, true, false, 101);
        assert_eq!(c.operations["fast"].id, 41);
        // Host changes leave both business generations intact.
        let mut host = c.config.clone();
        host.revision = "host-only".into();
        host.mqtt_bridge_enabled = !host.mqtt_bridge_enabled;
        c.request_config(host);
        assert_eq!(c.config.device_revision("fast"), revision);
        schedule(&mut c, &fast, true, false, 102);
        assert_eq!(c.operations["fast"].id, 41);
    }

    fn fixture() -> (Controller, Device) {
        let mut op = super::super::tests::operation(OperationPhase::Completed, false);
        op.started = 0;
        op.completed_at = None;
        op.results
            .insert("wifi".into(), json!({"id":"wifi","state":"verified"}));
        let device = op.device.clone();
        crate::omci_auth::test_authorize(&device.optical_sn);
        let mut c = Controller::default();
        c.next_id = 100;
        c.config.enabled = true;
        c.config.automatic = true;
        c.config.revision = "new-revision".into();
        c.config.triggers.insert("config_change".into());
        c.config.devices.push(device.clone());
        c.operations.insert(device.id.clone(), op);
        (c, device)
    }

    #[test]
    fn missing_volatile_approval_allows_only_reads_and_respects_automatic_policy() {
        let (mut c, device) = fixture();
        c.operations.clear();
        c.load_snapshots();
        c.startup_pending.insert(device.id.clone());
        c.config.triggers.insert("startup".into());
        c.config.automatic = false;
        schedule(&mut c, &device, true, false, 100);
        assert!(c.operations.is_empty());
        c.config.automatic = true;
        schedule(&mut c, &device, true, false, 101);
        assert!(!c.operations[&device.id].apply);
        let read_id = c.operations[&device.id].id;
        c.operations.get_mut(&device.id).unwrap().complete(102);
        schedule(&mut c, &device, true, true, 103);
        assert_eq!(c.operations[&device.id].id, read_id);
        assert!(!crate::provisioning::runtime_store::approved(
            &device.optical_sn
        ));
        c.start_selected(&device.id, true, false, None).unwrap();
        assert!(crate::provisioning::runtime_store::approved(
            &device.optical_sn
        ));
        assert!(c.operations[&device.id].apply);
    }

    #[test]
    fn changed_revision_is_scheduled_per_device_when_it_becomes_reachable() {
        let (mut c, ready) = fixture();
        let mut offline = ready.clone();
        offline.id = "offline".into();
        offline.optical_sn = "H3CT00000002".into();
        offline.cwmp_id = "offline-id".into();
        c.config.devices.push(offline.clone());
        let mut previous = c.operations[&ready.id].view();
        previous["device"] = json!(offline.id);
        previous["optical_sn"] = json!(offline.optical_sn);
        c.recovered.insert(offline.id.clone(), previous);

        schedule(&mut c, &offline, false, false, 100);
        schedule(&mut c, &ready, true, false, 100);
        assert_eq!(c.operations[&ready.id].revision, "new-revision");
        assert!(c.operations[&ready.id].apply);
        assert!(!c.operations.contains_key(&offline.id));
        assert_eq!(c.recovered[&offline.id]["revision"], "rev");
        let id = c.operations[&ready.id].id;
        schedule(&mut c, &ready, true, false, 101);
        assert_eq!(c.operations[&ready.id].id, id);

        crate::omci_auth::test_authorize(&offline.optical_sn);
        schedule(&mut c, &offline, true, false, 102);
        assert_eq!(c.operations[&offline.id].revision, "new-revision");
    }

    #[test]
    fn revision_trigger_survives_restart_and_does_not_repeat_after_completion() {
        let (c, device) = fixture();
        let mut restarted = Controller::default();
        restarted.config = c.config.clone();
        restarted.recover(c.history().as_bytes());
        schedule(&mut restarted, &device, true, false, 100);
        assert_eq!(restarted.operations[&device.id].revision, "new-revision");
        let op = restarted.operations.get_mut(&device.id).unwrap();
        op.results
            .insert("wifi".into(), json!({"id":"wifi","state":"verified"}));
        op.index = op.device.items.len();
        op.complete(101);
        let id = op.id;
        schedule(&mut restarted, &device, true, false, 102);
        assert_eq!(restarted.operations[&device.id].id, id);
    }

    #[test]
    fn config_change_preserves_unknown_write_and_disabled_trigger_guards() {
        let (mut c, device) = fixture();
        c.operations
            .get_mut(&device.id)
            .unwrap()
            .results
            .insert("wifi".into(), json!({"id":"wifi","state":"unknown"}));
        schedule(&mut c, &device, true, false, 100);
        assert_eq!(c.operations[&device.id].revision, "rev");
        c.operations.get_mut(&device.id).unwrap().results.clear();
        c.config.triggers.clear();
        schedule(&mut c, &device, true, false, 101);
        assert_eq!(c.operations[&device.id].revision, "rev");
    }

    #[test]
    fn read_only_plan_drift_does_not_schedule_an_apply() {
        let (mut c, device) = fixture();
        c.config.triggers.clear();
        let op = c.operations.get_mut(&device.id).unwrap();
        op.apply = false;
        op.results
            .insert("wifi".into(), json!({"id":"wifi","state":"drift"}));
        let id = op.id;
        schedule(&mut c, &device, true, false, 100);
        assert_eq!(c.operations[&device.id].id, id);
        assert!(!c.operations[&device.id].apply);
        assert!(!c.attempts.contains_key(&device.id));
    }

    #[test]
    fn automatic_causes_share_one_budget_and_keep_backoff_from_completion() {
        let (mut c, device) = fixture();
        c.config
            .triggers
            .extend(["device_online".into(), "cwmp_boot".into()]);
        c.attempts.insert(device.id.clone(), 4);
        c.operations.get_mut(&device.id).unwrap().complete(100);
        schedule(&mut c, &device, true, true, 200);
        assert_eq!(c.operations[&device.id].revision, "rev");
        assert_eq!(c.next_retry_at[&device.id], 220);
        assert_ne!(c.automatic_ready[&device.id].0, 0);
        // Re-observation cannot push the completion-anchored deadline forward.
        schedule(&mut c, &device, true, false, 219);
        assert_eq!(c.next_retry_at[&device.id], 220);
        schedule(&mut c, &device, true, false, 220);
        assert_eq!(c.attempts[&device.id], 5);
        let op = c.operations.get_mut(&device.id).unwrap();
        op.results.insert("wifi".into(), json!({"state":"blocked"}));
        op.started = 220;
        op.complete(1000);
        let id = op.id;
        for time in [1001, 2000, 10000] {
            schedule(&mut c, &device, true, true, time);
            assert_eq!(c.operations[&device.id].id, id);
            assert_eq!(c.attempts[&device.id], 5);
        }
    }

    #[test]
    fn verified_explicit_apply_consumes_preexisting_startup_and_ready_causes() {
        let (mut c, device) = fixture();
        c.config.triggers = ["startup".into(), "config_change".into()].into();
        c.startup_pending.insert(device.id.clone());
        c.automatic_ready.insert(device.id.clone(), Reasons(3));
        let op = c.operations.get_mut(&device.id).unwrap();
        op.revision = c.config.device_revision(&device.id).to_owned();
        op.results
            .insert("wifi".into(), json!({"state":"verified"}));
        op.complete(100);
        let id = op.id;
        schedule(&mut c, &device, true, false, 101);
        assert_eq!(c.operations[&device.id].id, id);
        assert!(!c.startup_pending.contains(&device.id));
        assert_eq!(c.automatic_ready[&device.id].0, 0);
    }

    #[test]
    fn safe_failed_apply_retries_without_needing_a_second_external_trigger() {
        let (mut c, device) = fixture();
        c.config.triggers.clear();
        c.attempts.insert(device.id.clone(), 1);
        let op = c.operations.get_mut(&device.id).unwrap();
        op.revision = "new-revision".into();
        op.results.insert("wifi".into(), json!({"state":"blocked"}));
        op.complete(100);
        let id = op.id;
        schedule(&mut c, &device, true, false, 114);
        assert_eq!(c.operations[&device.id].id, id);
        schedule(&mut c, &device, true, false, 115);
        assert_ne!(c.operations[&device.id].id, id);
        assert_eq!(c.attempts[&device.id], 2);
    }

    #[test]
    fn interval_zero_read_failure_retries_only_reads_with_its_own_finite_budget() {
        let (mut c, device) = fixture();
        c.config.triggers.clear();
        c.config.interval_seconds = 0;
        c.attempts.insert(device.id.clone(), 5);
        let op = c.operations.get_mut(&device.id).unwrap();
        op.apply = false;
        op.results.insert("wifi".into(), json!({"state":"blocked"}));
        op.complete(100);
        let old = op.id;
        schedule(&mut c, &device, true, false, 114);
        assert_eq!(c.operations[&device.id].id, old);
        for (i, time) in [(1, 115), (2, 145), (3, 205)] {
            schedule(&mut c, &device, true, false, time);
            let op = c.operations.get_mut(&device.id).unwrap();
            assert!(!op.apply);
            assert_eq!(c.read_recovery[&device.id].attempts, i);
            assert_eq!(c.attempts[&device.id], 5);
            op.results.insert("wifi".into(), json!({"state":"blocked"}));
            op.completed_at = None;
            op.complete(time);
        }
        let last = c.operations[&device.id].id;
        schedule(&mut c, &device, true, false, 10000);
        assert_eq!(c.operations[&device.id].id, last);
        assert_eq!(c.read_recovery[&device.id].attempts, 3);
    }

    #[test]
    fn read_recovery_never_replaces_unknown_and_success_does_not_rearm_write_budget() {
        let (mut c, device) = fixture();
        c.config.triggers.clear();
        c.attempts.insert(device.id.clone(), 5);
        c.read_recovery.insert(
            device.id.clone(),
            snapshots::ReadRecovery {
                attempts: 2,
                retry_at: 100,
            },
        );
        let op = c.operations.get_mut(&device.id).unwrap();
        op.apply = false;
        op.revision = "new-revision".into();
        op.results.insert("wifi".into(), json!({"state":"unknown"}));
        op.complete(100);
        let id = op.id;
        schedule(&mut c, &device, true, false, 1000);
        assert_eq!(c.operations[&device.id].id, id);
        assert_eq!(c.read_recovery[&device.id].attempts, 2);
        c.operations
            .get_mut(&device.id)
            .unwrap()
            .results
            .insert("wifi".into(), json!({"state":"verified"}));
        schedule(&mut c, &device, true, false, 1001);
        assert!(!c.read_recovery.contains_key(&device.id));
        assert_eq!(c.attempts[&device.id], 5);
    }

    #[test]
    fn restart_of_an_inflight_read_does_not_bypass_its_recovered_budget() {
        let (mut c, device) = fixture();
        c.config.triggers.clear();
        let op = c.operations.get_mut(&device.id).unwrap();
        op.apply = false;
        op.stage = OperationPhase::Reading;
        let history = c.history();
        c.operations.clear();
        c.recover(history.as_bytes());
        c.read_recovery.insert(
            device.id.clone(),
            snapshots::ReadRecovery {
                attempts: 3,
                retry_at: 200,
            },
        );
        let id = c.recovered[&device.id]["operation_id"].clone();
        for time in [1000, 10000] {
            schedule(&mut c, &device, true, false, time);
            assert!(c.operations.is_empty());
            assert_eq!(c.recovered[&device.id]["operation_id"], id);
            assert_eq!(c.read_recovery[&device.id].attempts, 3);
        }
    }

    #[test]
    fn only_current_verified_apply_settles_the_automatic_episode() {
        for plan in [false, true] {
            for current in [false, true] {
                let (mut c, device) = fixture();
                c.config.triggers.clear();
                c.attempts.insert(device.id.clone(), 5);
                let op = c.operations.get_mut(&device.id).unwrap();
                op.apply = !plan;
                if current {
                    op.revision = "new-revision".into();
                }
                op.complete(100);
                schedule(&mut c, &device, true, false, 1000);
                assert_eq!(c.attempts.contains_key(&device.id), plan || !current);
            }
        }
    }

    #[test]
    fn drained_configuration_reload_does_not_rearm_an_exhausted_episode() {
        let (mut c, device) = fixture();
        c.attempts.insert(device.id.clone(), 5);
        c.next_retry_at.insert(device.id.clone(), 900);
        let mut blocker = super::super::tests::operation(OperationPhase::Writing, false);
        blocker.device.id = "busy".into();
        c.operations.insert("busy".into(), blocker);
        let mut next = c.config.clone();
        next.revision = "latest".into();
        c.request_config(next);
        c.operations.get_mut("busy").unwrap().complete(101);
        assert!(c.activate_pending_config());
        assert_eq!(c.attempts[&device.id], 5);
        schedule(&mut c, &device, true, true, 10000);
        assert_eq!(c.operations[&device.id].revision, "rev");
        assert_eq!(c.attempts[&device.id], 5);
    }

    #[test]
    fn pending_reload_prevents_old_revision_rescheduling_then_starts_latest_immediately() {
        let (mut c, device) = fixture();
        let mut blocker = super::super::tests::operation(OperationPhase::Writing, false);
        blocker.device.id = "busy".into();
        let mut next = c.config.clone();
        next.revision = "latest".into();
        c.operations.insert("busy".into(), blocker);
        c.request_config(next);
        schedule(&mut c, &device, true, false, 100);
        assert_eq!(c.operations[&device.id].revision, "rev");
        c.operations.get_mut("busy").unwrap().complete(101);
        assert!(c.activate_pending_config());
        schedule(&mut c, &device, true, false, 101);
        assert_eq!(c.operations[&device.id].revision, "latest");
        assert!(c.operations[&device.id].apply);
    }
}

fn identity(
    d: &Device,
    observed: Option<&crate::cwmp::observations::DeviceObservation>,
) -> crate::cwmp::identity::Identity {
    crate::cwmp::identity::Identity {
        optical_vendor: d.optical_sn[..4].into(),
        optical_serial: d.optical_sn.clone(),
        cwmp_serial: observed.map(|d| d.serial.clone()).unwrap_or_default(),
        product_class: observed
            .map(|d| d.product_class.clone())
            .unwrap_or_default(),
        software_version: observed
            .map(|d| d.software_version.clone())
            .unwrap_or_default(),
        hardware_version: observed
            .map(|d| d.hardware_version.clone())
            .unwrap_or_default(),
    }
}

/// False retains the original tick's early-continue boundary (no ownership publication).
fn reconcile(
    op: &mut Operation,
    identity: crate::cwmp::identity::Identity,
    epoch: u64,
    reachable: bool,
    reload_pending: bool,
) -> bool {
    let Execution::Acs(state) = &mut op.execution else {
        return false;
    };
    state
        .capabilities
        .tick(&op.device.cwmp_id, identity, epoch, reachable, acs);
    if op.capabilities.blocked() {
        let reason = op.capabilities.view()["reason"]
            .as_str()
            .unwrap_or("capability contract unresolved")
            .to_owned();
        for item in &op.device.items {
            op.results.entry(item.id.clone()).or_insert_with(|| json!({"id":item.id,"kind":item.kind,"service_type":item.service_type,"state":"blocked","reason":reason,"revision":op.revision}));
        }
        op.complete(now());
        return false;
    }
    if !op.capabilities.ready() {
        return false;
    }
    op.device.resolved_contract = Some(op.capabilities.selected().unwrap().to_owned());
    if !op.compiled {
        match op.capabilities.endpoints(&op.device) {
            Ok(endpoints) => {
                crate::cwmp::compiler::compile_device(&mut op.device, &endpoints);
                let mut prepared = Operation::new(
                    op.id,
                    op.device.clone(),
                    op.revision.clone(),
                    op.apply,
                    op.management,
                );
                prepared.wan = wan::Reconciler::new(
                    &prepared.device,
                    op.wan.owned.clone(),
                    op.wan.full_sync(),
                );
                prepared.wifi = wifi::Reconciler::new(
                    &prepared.device,
                    op.wifi.registry.clone(),
                    op.wan.full_sync(),
                );
                op.device = prepared.device;
                let Execution::Acs(state) = prepared.execution else {
                    unreachable!()
                };
                op.wan = state.wan;
                op.wifi = state.wifi;
                op.compiled = true;
            }
            Err(reason) => {
                op.capabilities.invalidate(&reason);
                return false;
            }
        }
    }
    op.wan.protected_roots = op.capabilities.protected_wan();
    op.wan.evidence_paths = op.capabilities.wan_fields();
    let Execution::Acs(state) = &mut op.execution else {
        unreachable!()
    };
    let ready = state
        .wan
        .tick(&mut op.device, op.apply, op.management, reachable);
    if let Err(error) = op.wan.save(&op.device.cwmp_id) {
        op.wan.results.insert(
            "persistence".into(),
            json!({"state":"blocked","reason":error}),
        );
        op.complete(now());
    } else if ready {
        let Execution::Acs(state) = &op.execution else {
            unreachable!()
        };
        state.wan.block_unresolved(&mut op.device);
        let finish = op.index >= op.device.items.len();
        if finish
            && op
                .wan
                .results
                .get("final_inventory")
                .is_some_and(|r| ItemOutcome::from_result(r) == Some(ItemOutcome::Blocked))
        {
            for item in op.device.items.iter().filter(|i| i.wan_ref.is_some()) {
                if let Some(result) = op.results.get_mut(&item.id) {
                    result["state"] = json!("blocked");
                    result["reason"] =
                        json!("final full WAN inventory does not match ordered configuration");
                }
            }
        }
        let verified = op
            .device
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Ssid)
            .all(|i| {
                op.results
                    .get(&i.id)
                    .is_some_and(|r| ItemOutcome::from_result(r) == Some(ItemOutcome::Verified))
            });
        let Execution::Acs(state) = &mut op.execution else {
            unreachable!()
        };
        if finish && state.wan.begin_final_verification(op.apply) {
            op.stage = OperationPhase::WanVerifying;
        } else if state
            .wifi
            .tick(&op.device, op.apply, reachable, finish, verified)
        {
            if finish {
                op.complete(now());
            } else {
                let Execution::Acs(state) = &op.execution else {
                    unreachable!()
                };
                state.wifi.block_unresolved(&mut op.device);
                if matches!(
                    op.stage,
                    OperationPhase::WanReconciling | OperationPhase::WifiReconciling
                ) {
                    op.stage = OperationPhase::Waiting;
                }
                advance_or_supersede(op, reachable, reload_pending);
            }
        } else {
            op.stage = OperationPhase::WifiReconciling;
        }
        let Execution::Acs(state) = &mut op.execution else {
            unreachable!()
        };
        for item in &op.device.items {
            if op.results.get(&item.id).is_some_and(|r| {
                ItemOutcome::from_result(r) == Some(ItemOutcome::Verified)
                    && r["revision"] == op.revision
            }) {
                if let Some(owned) = state.wan.owned.get_mut(&item.id) {
                    if let Some(p) = item.parameters.iter().find(|p| {
                        p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST")
                    }) {
                        owned.service_list = p.value.clone();
                    }
                }
            }
        }
    } else {
        op.stage = OperationPhase::WanReconciling;
    }
    true
}

// Called only after WAN and Wi-Fi lifecycle transactions are drained.
fn advance_or_supersede(op: &mut Operation, reachable: bool, reload_pending: bool) {
    if reload_pending
        && op.stage == OperationPhase::Waiting
        && op.task.is_none()
        && op.submission.is_none()
    {
        op.supersede_remaining();
    } else {
        advance(op, reachable);
    }
}

#[cfg(test)]
mod reload_boundary_tests {
    use super::*;

    #[test]
    fn safe_boundary_skips_old_work_even_offline_without_losing_unknown_results() {
        let mut op = super::super::tests::operation(OperationPhase::Waiting, false);
        op.task = None;
        op.results.insert(
            "previous".into(),
            json!({"id":"previous","state":"unknown"}),
        );
        advance_or_supersede(&mut op, false, true);
        assert_eq!(op.stage, OperationPhase::Completed);
        assert_eq!(op.results["previous"]["state"], "unknown");
        assert_eq!(op.results["wifi"]["state"], "blocked");
        assert!(op.task.is_none());
    }

    #[test]
    fn reload_never_discards_an_inflight_write() {
        let mut op = super::super::tests::operation(OperationPhase::Writing, false);
        RPC.with(|r| {
            *r.borrow_mut() = (
                std::collections::VecDeque::from([json!({"state":"sent"})]),
                Vec::new(),
            )
        });
        advance_or_supersede(&mut op, true, true);
        assert_eq!(op.stage, OperationPhase::Writing);
        assert_eq!(op.task, Some(1));
        assert!(op.completed_at.is_none());
    }

    #[test]
    fn disabled_cwmp_reports_disabled_management_state() {
        let mut d = super::super::tests::operation(OperationPhase::Completed, false).device;
        d.bootstrap_adapter = "none".into();
        d.provisioning_adapter = "hl202-mqtt".into();
        assert!(!d.cwmp_enabled());

        d.provisioning_adapter = "none".into();
        assert!(!d.cwmp_enabled());

        d.bootstrap_adapter = "manual-acs".into();
        assert!(d.cwmp_enabled());

        d.bootstrap_adapter = "none".into();
        d.provisioning_adapter = "hl202-cu".into();
        assert!(d.cwmp_enabled());
    }
}
