use super::*;

#[test]
#[cfg_attr(not(feature = "device-hl202"), ignore = "requires HL202 adapter")]
fn connection_request_toggle_rechecks_verified_settings_when_only_flag_changes() {
    for automatic in [false, true] {
        for enabled in [false, true] {
            let mut device = Device {
                id: "interval".into(),
                optical_sn: "H3CT12345678".into(),
                cwmp_id: "interval".into(),
                omci_adapter: "hl202".into(),
                bootstrap_adapter: "hl202-mqtt-acs".into(),
                connection_request: Default::default(),
                provisioning_adapter: "hl202-cu".into(),
                resolved_contract: None,
                items: Vec::new(),
                wifi_cleanup: "preserve".into(),
                wifi_preserve: Default::default(),
            };
            device.connection_request.enabled = enabled;
            let management = Management {
                vlans: vec![46],
                vlan: 46,
                priority: 0,
                acs_url: "http://192.0.2.1/cwmp".into(),
            };
            let mac = "02AABBCCDDEE";
            let mut sources = Registry::new("interval-test".into());
            sources.update_topology(
                BTreeMap::from([(1, device.optical_sn.clone())]),
                Ok(BTreeMap::from([(mac.into(), device.optical_sn.clone())])),
                100,
            );
            sources.observe(mac, 100);
            let mut coordinator = Coordinator::default();
            let observe = |device, time, at| Observation {
                device,
                management: Some(&management),
                last_inform: at,
                online: true,
                new_boot: false,
                freshness: 600,
                time,
                acs_available: true,
                authorization: Some((1, automatic)),
            };
            let first = coordinator
                .schedule(observe(&device, 100, None), &sources)
                .unwrap();
            coordinator.records.get_mut(&device.id).unwrap().finish(
                first.generation,
                Ok(true),
                101,
            );
            assert!(coordinator
                .schedule(observe(&device, 103, Some(102)), &sources)
                .is_none());
            assert_eq!(coordinator.records[&device.id].state, Phase::Ready);

            let mut toggled = device.clone();
            toggled.connection_request.enabled = !enabled;
            let next = coordinator
                .schedule(observe(&toggled, 104, Some(102)), &sources)
                .expect(
                    "flag-only reload must schedule settings reconciliation even in manual mode",
                );

            assert_ne!(next.generation, first.generation);
            assert!(next.persist_matching);
            assert_eq!(coordinator.records[&device.id].settings_verified_at, None);
            assert_eq!(coordinator.records[&device.id].state, Phase::Reading);
        }
    }
}
