//! Host-owned resources. ONU protocol IDs are not Linux ports or GEM identifiers.
use crate::pon_engine::onu_id;
use std::{collections::BTreeSet, io};

/// Semantic MAC readback; raw registers are optional platform evidence.
#[derive(Clone, Copy, Debug)]
pub struct PonMacReadback {
    pub raw: Option<u32>,
    pub tx_enabled: Option<bool>,
    pub fec_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OnuResources {
    pub onu_id: u8,
    pub alloc_id: u16,
    pub gem_id: u16,
    /// Optional Linux port association; independent of slot order and port count.
    pub interface: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub struct Resources {
    pub onu_ids: &'static [u8],
    pub management_interfaces: &'static [&'static str],
    pub capture_interface: Option<&'static str>,
    pub downstream_interfaces: &'static [&'static str],
    pub allocations: &'static [OnuResources],
}

impl Resources {
    /// Validate the platform definition before it can supply IDs to the allocator.
    /// Alloc/GEM values and Linux interface associations remain platform policies.
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = BTreeSet::new();
        for &id in self.onu_ids {
            if !onu_id::is_valid(u16::from(id)) {
                return Err(format!("ONU-ID {id} is outside the protocol range"));
            }
            if !ids.insert(id) {
                return Err(format!("duplicate available ONU-ID {id}"));
            }
        }
        let mut assigned = BTreeSet::new();
        for entry in self.allocations {
            let id = entry.onu_id;
            if !onu_id::is_valid(u16::from(id)) {
                return Err(format!(
                    "allocation ONU-ID {id} is outside the protocol range"
                ));
            }
            if !assigned.insert(id) {
                return Err(format!("duplicate allocation for ONU-ID {id}"));
            }
            if !ids.contains(&id) {
                return Err(format!("allocation ONU-ID {id} is not available"));
            }
        }
        if let Some(id) = ids.difference(&assigned).next() {
            return Err(format!("ONU-ID {id} has no platform resource assignment"));
        }
        Ok(())
    }

    pub fn onu(&self, id: u8) -> io::Result<OnuResources> {
        self.allocations
            .iter()
            .find(|entry| entry.onu_id == id)
            .copied()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ONU has no platform resource assignment",
                )
            })
    }

    pub fn interface(&self, id: u8) -> Option<&'static str> {
        self.onu(id).ok()?.interface
    }

    pub fn owns_gem(&self, gem: u16) -> bool {
        self.allocations.iter().any(|entry| entry.gem_id == gem)
    }

    pub fn validate_pins(
        &self,
        pins: &std::collections::BTreeMap<String, u8>,
    ) -> Result<(), String> {
        let mut assigned = std::collections::BTreeSet::new();
        // Identities are serial-number material: keep them out of every message.
        for (_identity, id) in pins {
            if !self.onu_ids.contains(id) {
                return Err("configured ONU-ID is unsupported by the selected platform".into());
            }
            if !assigned.insert(*id) {
                return Err(format!("duplicate pin assignment for ONU-ID {id}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: OnuResources = OnuResources {
        onu_id: 3,
        alloc_id: 900,
        gem_id: 1200,
        interface: None,
    };
    const SECOND: OnuResources = OnuResources {
        onu_id: 72,
        alloc_id: 901,
        gem_id: 1201,
        interface: Some("shared-port"),
    };
    const VALID: Resources = Resources {
        onu_ids: &[3, 72],
        management_interfaces: &[],
        capture_interface: None,
        downstream_interfaces: &["shared-port"],
        allocations: &[SECOND, FIRST],
    };

    #[test]
    fn valid_sparse_resources_do_not_require_slot_order_or_one_port_per_onu() {
        VALID.validate().unwrap();
        assert_eq!(VALID.onu(72).unwrap().gem_id, 1201);
        assert_eq!(VALID.interface(3), None);
        let shared = Resources {
            allocations: &[
                OnuResources {
                    interface: Some("shared-port"),
                    ..FIRST
                },
                SECOND,
            ],
            ..VALID
        };
        shared.validate().unwrap();
        assert_eq!(shared.interface(3), shared.interface(72));
    }

    #[test]
    fn invalid_resources_reject_reserved_duplicate_missing_and_extra_onu_ids() {
        let cases = [
            (
                Resources {
                    onu_ids: &[254],
                    ..VALID
                },
                "outside the protocol range",
            ),
            (
                Resources {
                    onu_ids: &[255],
                    ..VALID
                },
                "outside the protocol range",
            ),
            (
                Resources {
                    onu_ids: &[3, 3],
                    ..VALID
                },
                "duplicate available ONU-ID 3",
            ),
            (
                Resources {
                    allocations: &[FIRST, FIRST],
                    ..VALID
                },
                "duplicate allocation for ONU-ID 3",
            ),
            (
                Resources {
                    allocations: &[FIRST],
                    ..VALID
                },
                "ONU-ID 72 has no platform resource assignment",
            ),
            (
                Resources {
                    onu_ids: &[3],
                    ..VALID
                },
                "allocation ONU-ID 72 is not available",
            ),
            (
                Resources {
                    allocations: &[OnuResources {
                        onu_id: 254,
                        ..FIRST
                    }],
                    ..VALID
                },
                "outside the protocol range",
            ),
            (
                Resources {
                    allocations: &[OnuResources {
                        onu_id: 255,
                        ..FIRST
                    }],
                    ..VALID
                },
                "outside the protocol range",
            ),
        ];
        for (resources, expected) in cases {
            let error = resources.validate().unwrap_err();
            assert!(error.contains(expected), "{resources:?}: {error}");
        }
    }

    #[test]
    fn pin_validation_rejects_unsupported_and_duplicate_ids_without_exposing_identities() {
        let pins =
            |entries: &[(&str, u8)]| entries.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        assert!(VALID.validate_pins(&pins(&[("AAA", 3)])).is_ok());
        let error = VALID.validate_pins(&pins(&[("AAA", 7)])).unwrap_err();
        assert!(error.contains("unsupported by the selected platform"));
        let error = VALID
            .validate_pins(&pins(&[("AAA", 3), ("BBB", 3)]))
            .unwrap_err();
        assert!(error.contains("duplicate pin assignment for ONU-ID 3"));
    }
}
