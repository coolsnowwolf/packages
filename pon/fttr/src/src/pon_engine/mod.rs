//! Modular GPON Micro-OLT Protocol Engine
//!
//! Provides decoupled protocol processing:
//! - `ploam`: ITU-T G.984.3 O1-O5 discovery & ranging state machine
//! - `omci`: ITU-T G.988 MIB baseline provisioning

pub mod omci;
pub mod onu_id;
pub mod ploam;

use self::ploam::{GponOnu, OnuState, PloamFrame};

#[cfg(test)]
mod test_resources;
#[cfg(test)]
pub(crate) fn test_engine() -> PonEngine {
    PonEngine::new(&test_resources::RESOURCES)
}

pub struct PonEngine {
    pub resources: &'static crate::platform::resources::Resources,
    pub active_onus: Vec<GponOnu>,
    pub(crate) onu_pins: std::collections::BTreeMap<String, u8>,
    pub(crate) pin_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const ONU_SLOTS: u8 = 16;

    #[test]
    fn pins_reserve_offline_slots_and_survive_reordered_discovery_after_restart() {
        let pins = std::collections::BTreeMap::from([
            ("TEST00000001".into(), 0),
            ("TEST00000002".into(), 15),
        ]);
        for order in [[1, 2], [2, 1]] {
            let mut engine = test_engine();
            engine.onu_pins = pins.clone();
            assert!(engine.active_onus.is_empty());
            // An automatic ONU arriving first cannot steal either reservation.
            assert_eq!(
                engine.register_onu(*b"TEST", [0, 0, 0, 3]).unwrap().onu_id,
                1
            );
            for serial in order {
                let id = if serial == 1 { 0 } else { 15 };
                assert_eq!(
                    engine
                        .register_onu(*b"TEST", [0, 0, 0, serial])
                        .unwrap()
                        .onu_id,
                    id
                );
                engine.mark_offline(id);
                assert_eq!(
                    engine
                        .register_onu(*b"TEST", [0, 0, 0, serial])
                        .unwrap()
                        .onu_id,
                    id
                );
            }
            assert_eq!(engine.active_onus.len(), 3);
        }
    }

    #[test]
    fn all_reserved_slots_reject_unpinned_identity_and_bad_startup_blocks_allocation() {
        let mut engine = test_engine();
        engine.onu_pins = (0..ONU_SLOTS)
            .map(|id| (format!("TEST000000{id:02X}"), id))
            .collect();
        assert!(engine.register_onu(*b"FREE", [0; 4]).is_none());
        for id in (0..ONU_SLOTS).rev() {
            assert_eq!(
                engine.register_onu(*b"TEST", [0, 0, 0, id]).unwrap().onu_id,
                id
            );
        }
        engine.pin_error = Some("invalid startup configuration".into());
        assert!(engine.register_onu(*b"TEST", [0; 4]).is_none());
    }

    #[test]
    fn all_sixteen_slots_are_unique_and_full_table_preserves_assignments() {
        let mut engine = test_engine();
        for id in 0..ONU_SLOTS {
            let onu = engine.register_onu(*b"TEST", [0, 0, 0, id]).unwrap();
            assert_eq!(onu.onu_id, id);
            let assign = engine.activate_onu(id);
            assert_eq!(assign[0].payload[0], id);
        }
        assert!(engine.register_onu(*b"TEST", [0, 0, 0, 16]).is_none());
        for id in 0..ONU_SLOTS {
            assert_eq!(
                engine.register_onu(*b"TEST", [0, 0, 0, id]).unwrap().onu_id,
                id
            );
        }
        assert_eq!(engine.active_onus.len(), 16);
        engine.mark_offline(15);
        assert!(engine.register_onu(*b"TEST", [0, 0, 0, 16]).is_none());
        assert_eq!(
            engine.register_onu(*b"TEST", [0, 0, 0, 15]).unwrap().onu_id,
            15
        );
    }

    #[test]
    fn serial_and_assignment_do_not_claim_operation() {
        let mut engine = test_engine();
        let onu = engine
            .register_onu(*b"TEST", [0x12, 0x34, 0x56, 0x78])
            .unwrap();
        assert_eq!(onu.state, OnuState::O3SerialNumber);
        let frames = engine.activate_onu(onu.onu_id);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].message_id, ploam::PLOAM_ASSIGN_ONU_ID);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        assert_eq!(engine.active_onus[0].firmware_version, "Unknown");
    }

    #[test]
    fn sparse_ids_and_unrelated_alloc_gem_and_netdev_do_not_alias() {
        use crate::platform::resources::{OnuResources, Resources};
        static RESOURCES: Resources = Resources {
            onu_ids: &[3, 72],
            management_interfaces: &[],
            capture_interface: None,
            downstream_interfaces: &["pon-subscriber-a", "uplink-b"],
            allocations: &[
                OnuResources {
                    onu_id: 3,
                    alloc_id: 900,
                    gem_id: 1200,
                    interface: Some("pon-subscriber-a"),
                },
                OnuResources {
                    onu_id: 72,
                    alloc_id: 901,
                    gem_id: 1201,
                    interface: Some("uplink-b"),
                },
            ],
        };
        let mut engine = PonEngine::new(&RESOURCES);
        engine.onu_pins.insert("TEST00000001".into(), 72);
        assert_eq!(
            engine.register_onu(*b"TEST", [0, 0, 0, 2]).unwrap().onu_id,
            3
        );
        assert_eq!(
            engine.register_onu(*b"TEST", [0, 0, 0, 1]).unwrap().onu_id,
            72
        );
        assert!(engine.register_onu(*b"TEST", [0, 0, 0, 3]).is_none());
        assert_eq!(RESOURCES.interface(72), Some("uplink-b"));
        assert_eq!(RESOURCES.onu(72).unwrap().alloc_id, 901);
        assert_eq!(RESOURCES.onu(72).unwrap().gem_id, 1201);
        assert!(RESOURCES.onu(0).is_err());
        assert!(!RESOURCES.owns_gem(0x100 + 72));
        engine.mark_offline(72);
        assert_eq!(
            engine.register_onu(*b"TEST", [0, 0, 0, 1]).unwrap().onu_id,
            72
        );
        engine.onu_pins.insert("TEST00000003".into(), 0);
        assert!(engine.try_register_onu(*b"TEST", [0, 0, 0, 3]).is_err());
    }

    #[test]
    fn receiver_rejects_bad_crc_and_truncated_messages() {
        let frame = PloamFrame::new(0xff, 1, [0x54, 0x45, 0x53, 0x54, 1, 2, 3, 4, 0, 0]);
        let mut bytes = frame.to_bytes();
        assert!(PloamFrame::from_bytes(&bytes).is_some());
        assert!(PloamFrame::from_bytes(&bytes[..12]).is_none());
        bytes[6] ^= 1;
        assert!(PloamFrame::from_bytes(&bytes).is_none());
    }

    #[test]
    fn captured_ranging_vector_and_corrupt_post_ranging_frames() {
        // Independent device trace: slot 1 measurement 0x3c9b8, absolute
        // main path, bit units. The received bad ACK must remain rejected.
        assert_eq!(
            PloamFrame::assign_delay(1, 248248).to_bytes(),
            [1, 4, 0, 0, 3, 0xc9, 0xb8, 0, 0, 0, 0, 0, 0xe8]
        );
        assert!(PloamFrame::from_bytes(&[
            1, 2, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x36, 0x38, 0x38, 0x26
        ])
        .is_none());
        assert!(
            PloamFrame::from_bytes(&[1, 9, 0x0e, 1, 0x0e, 1, 0, 0x10, 0, 0, 0, 1, 0xe2]).is_none()
        );
        // Native, unmodified frame after TN=10 / shift=0: CRC and copied
        // Assign_Alloc-ID payload both match, unlike the capture above.
        let ack =
            PloamFrame::from_bytes(&[1, 9, 0x0e, 1, 0x0e, 1, 0, 0x10, 0, 0, 0, 0, 0xe2]).unwrap();
        assert_eq!(ack.payload[0], 0x0e);
        assert_eq!(
            ack.payload[1..],
            PloamFrame::assign_alloc_id(1).to_bytes()[..9]
        );
    }

    #[test]
    fn retry_discards_previous_eqd_and_operation_state() {
        let mut engine = test_engine();
        let onu = engine.register_onu(*b"TEST", [1, 2, 3, 4]).unwrap();
        engine.active_onus[0].eq_delay = Some(248248);
        engine.active_onus[0].state = OnuState::O5Operation;
        engine.activate_onu(onu.onu_id);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
    }

    #[test]
    fn offline_retains_identity_but_requires_new_activation_evidence() {
        let mut engine = test_engine();
        let old = engine.register_onu(*b"TEST", [1, 2, 3, 4]).unwrap();
        engine.active_onus[0].state = OnuState::O5Operation;
        engine.active_onus[0].eq_delay = Some(248248);
        engine.mark_offline(old.onu_id);
        assert_eq!(engine.active_onus[0].state, OnuState::O1Initial);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        let again = engine
            .register_onu(old.vendor_id, old.serial_number)
            .unwrap();
        assert_eq!(again.onu_id, old.onu_id);
        assert_eq!(engine.active_onus.len(), 1);
        engine.activate_onu(again.onu_id);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
        assert_eq!(engine.active_onus[0].eq_delay, None);
    }

    #[test]
    fn platform_confirmation_uses_hardware_slot_and_retires_conflicting_identity() {
        let mut engine = test_engine();
        engine
            .confirm_platform_onu(3, *b"TEST", [0, 0, 0, 1], 42)
            .unwrap();
        assert_eq!(engine.active_onus[0].onu_id, 3);
        assert_eq!(engine.active_onus[0].state, OnuState::O5Operation);
        assert_eq!(engine.active_onus[0].eq_delay, Some(42));
        let displaced = engine
            .confirm_platform_onu(7, *b"TEST", [0, 0, 0, 1], 43)
            .unwrap();
        assert_eq!(displaced, vec![3]);
        assert_eq!(engine.active_onus.len(), 1);
        assert_eq!(engine.active_onus[0].onu_id, 7);
        let displaced = engine
            .confirm_platform_onu(7, *b"TEST", [0, 0, 0, 2], 44)
            .unwrap();
        assert_eq!(displaced, vec![7]);
        assert_eq!(engine.active_onus.len(), 1);
        assert_eq!(engine.active_onus[0].serial_number, [0, 0, 0, 2]);
        assert!(engine
            .confirm_platform_onu(254, *b"TEST", [0; 4], 0)
            .is_err());
    }
}

impl PonEngine {
    pub fn new(resources: &'static crate::platform::resources::Resources) -> Self {
        Self {
            resources,
            active_onus: Vec::new(),
            onu_pins: Default::default(),
            pin_error: None,
        }
    }

    /// Adopt an ID already confirmed by the platform, rather than allocating a
    /// second, potentially different ID in the shared engine.
    #[cfg(test)]
    pub(crate) fn confirm_platform_onu(
        &mut self,
        id: u8,
        vendor: [u8; 4],
        sn: [u8; 4],
        eqd: u32,
    ) -> Result<Vec<u8>, String> {
        self.confirm_platform_onu_with_rate(id, vendor, sn, eqd, None, None)
    }

    #[cfg(any(feature = "platform-fiberhome-an4475-g2", test))]
    pub(crate) fn confirm_platform_onu_with_rate(
        &mut self,
        id: u8,
        vendor: [u8; 4],
        sn: [u8; 4],
        eqd: u32,
        pon_mode: Option<&str>,
        upstream_phy_rate_gbps: Option<f32>,
    ) -> Result<Vec<u8>, String> {
        if !self.resources.onu_ids.contains(&id) {
            return Err(format!("platform ONU-ID {id} is unavailable"));
        }
        let displaced = self
            .active_onus
            .iter()
            .filter(|onu| {
                (onu.onu_id == id && (onu.vendor_id != vendor || onu.serial_number != sn))
                    || (onu.onu_id != id && onu.vendor_id == vendor && onu.serial_number == sn)
            })
            .map(|onu| onu.onu_id)
            .collect();
        self.active_onus.retain(|onu| {
            !((onu.onu_id == id && (onu.vendor_id != vendor || onu.serial_number != sn))
                || (onu.onu_id != id && onu.vendor_id == vendor && onu.serial_number == sn))
        });
        if !self.active_onus.iter().any(|onu| onu.onu_id == id) {
            self.active_onus.push(GponOnu::new(id, vendor, sn));
        }
        let onu = self
            .active_onus
            .iter_mut()
            .find(|onu| onu.onu_id == id)
            .unwrap();
        onu.eq_delay = Some(eqd);
        onu.pon_mode = pon_mode.map(str::to_owned);
        onu.upstream_phy_rate_gbps = upstream_phy_rate_gbps;
        onu.state = OnuState::O5Operation;
        Ok(displaced)
    }

    /// Processes an ONU discovered on the PON link (from upstream PLOAM or auto-ranging)
    pub fn register_onu(&mut self, vendor: [u8; 4], sn: [u8; 4]) -> Option<GponOnu> {
        self.try_register_onu(vendor, sn).ok()
    }

    pub(crate) fn try_register_onu(
        &mut self,
        vendor: [u8; 4],
        sn: [u8; 4],
    ) -> Result<GponOnu, String> {
        if let Some(error) = &self.pin_error {
            return Err(format!(
                "allocation blocked by startup configuration: {error}"
            ));
        }
        // Check if already registered
        for onu in &self.active_onus {
            if onu.vendor_id == vendor && onu.serial_number == sn {
                return Ok(onu.clone());
            }
        }

        let identity = GponOnu::new(0, vendor, sn)
            .full_sn_str()
            .to_ascii_uppercase();
        let id = match self.onu_pins.get(&identity) {
            Some(&id) if self.resources.onu_ids.contains(&id) => id,
            Some(&id) => {
                return Err(format!(
                    "reserved ONU-ID {id} is unavailable on this platform"
                ))
            }
            None => self
                .resources
                .onu_ids
                .iter()
                .copied()
                .find(|id| {
                    !self.onu_pins.values().any(|reserved| reserved == id)
                        && !self.active_onus.iter().any(|onu| onu.onu_id == *id)
                })
                .ok_or_else(|| {
                    format!(
                        "{identity}: all {} slots are occupied or reserved",
                        self.resources.onu_ids.len()
                    )
                })?,
        };
        if self.active_onus.iter().any(|onu| onu.onu_id == id) {
            return Err(format!(
                "{identity}: reserved ONU-ID {id} is already occupied"
            ));
        }

        let mut onu = GponOnu::new(id, vendor, sn);
        println!(
            "[+] PON Engine: New ONU discovered! ID={}, Vendor={}, SN={}",
            onu.onu_id,
            onu.vendor_str(),
            onu.sn_hex()
        );

        onu.state = OnuState::O3SerialNumber;
        self.active_onus.push(onu.clone());
        Ok(onu)
    }

    /// Forget activation evidence while retaining the serial-to-slot assignment.
    pub fn mark_offline(&mut self, onu_id: u8) {
        if let Some(onu) = self.active_onus.iter_mut().find(|o| o.onu_id == onu_id) {
            // Retain identity/slot only, never evidence from the previous activation.
            onu.state = OnuState::O1Initial;
            onu.eq_delay = None;
            onu.pon_mode = None;
            onu.upstream_phy_rate_gbps = None;
        }
    }

    /// Assign the ONU ID; fresh ranging and a validated reply must establish O5.
    pub fn activate_onu(&mut self, onu_id: u8) -> Vec<PloamFrame> {
        if let Some(onu) = self.active_onus.iter_mut().find(|o| o.onu_id == onu_id) {
            println!(
                "[*] PLOAM: Assigning ONU {}; awaiting measured ranging response",
                onu_id
            );
            onu.state = OnuState::O4Ranging;
            onu.eq_delay = None; // A new attempt must not inherit an old measurement.
            vec![PloamFrame::assign_onu_id(
                onu_id,
                onu.vendor_id,
                onu.serial_number,
            )]
        } else {
            Vec::new()
        }
    }
}
