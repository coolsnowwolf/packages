//! Bridge endpoints, a usable T-CONT and factory bridge/EVTO instances.
//!
//! A vendor-neutral profile cannot assume a fixed topology: ONTs arrive either
//! virgin (no bridge at all) or half provisioned by another OLT (bridge port
//! and EVTO already bound to the UNI). Everything the profile needs is read
//! from the uploaded MIB instead of being guessed.
use crate::pon_engine::omci::{attributes::AttributeValue, mib::MibReport};
use std::{collections::BTreeMap, io};

/// MAC bridge port already present in the ONU MIB.
#[derive(Clone, Copy, Debug)]
pub(super) struct ExistingPort {
    pub instance: u16,
    pub bridge: u16,
    pub port_num: u8,
}

pub(super) struct BridgeInventory {
    pub tcont: u16,
    pub endpoints: Vec<(u16, bool)>,
    /// Endpoint (UNI/VEIP) that already has a MAC bridge port in the ONU MIB.
    pub ports: BTreeMap<u16, ExistingPort>,
    /// Endpoint whose EVTO is already associated with it.
    pub evtos: BTreeMap<u16, u16>,
    /// Bridge port numbers already in use, so a new port never collides.
    pub used_port_numbers: Vec<u8>,
    /// GEM-side (TpType 5) bridge ports as (bridge port instance, GEM instance,
    /// PortNum). A binding that is not our own slot's GEM port is a leftover
    /// from a previous ONU-ID assignment; our own binding's PortNum is reused so
    /// reconfiguration is idempotent.
    pub gem_side: Vec<(u16, u16, u8)>,
}

impl BridgeInventory {
    pub fn from_mib(mib: &MibReport, alloc: u16, owned_allocs: &[u16]) -> io::Result<Self> {
        if !mib.uploaded_complete {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "generic bridge requires a complete MIB upload",
            ));
        }
        let mut tconts = Vec::new();
        let mut unis = Vec::new();
        let mut veips = Vec::new();
        let mut ports: BTreeMap<u16, ExistingPort> = BTreeMap::new();
        let mut evtos: BTreeMap<u16, u16> = BTreeMap::new();
        let mut used_port_numbers = Vec::new();
        let mut gem_side = Vec::new();
        for entity in &mib.entities {
            let attribute = |index: u8| {
                entity
                    .attributes
                    .iter()
                    .find(|attr| attr.definition.index == index)
                    .and_then(|attr| attr.value.as_ref())
                    .and_then(AttributeValue::unsigned)
            };
            match entity.class_id {
                262 => {
                    if let Some(value) = attribute(1) {
                        // Another current host allocation belongs to another service.
                        // Unassigned or foreign-OLT allocations remain reusable.
                        if value == u32::from(alloc)
                            || !owned_allocs.iter().any(|id| u32::from(*id) == value)
                        {
                            tconts.push((value != u32::from(alloc), entity.entity_id));
                        }
                    }
                }
                11 => unis.push(entity.entity_id),
                329 => veips.push(entity.entity_id),
                47 => {
                    let (Some(tp), Some(bridge), Some(kind)) =
                        (attribute(4), attribute(1), attribute(3))
                    else {
                        continue;
                    };
                    let (Ok(tp), Ok(bridge)) = (u16::try_from(tp), u16::try_from(bridge)) else {
                        continue;
                    };
                    let port_num = attribute(2).and_then(|value| u8::try_from(value).ok());
                    if let Some(port_num) = port_num {
                        used_port_numbers.push(port_num);
                    }
                    if kind == 5 {
                        gem_side.push((entity.entity_id, tp, port_num.unwrap_or(0)));
                    } else {
                        ports.insert(
                            tp,
                            ExistingPort {
                                instance: entity.entity_id,
                                bridge,
                                port_num: port_num.unwrap_or(0),
                            },
                        );
                    }
                }
                171 => {
                    let (Some(kind), Some(pointer)) = (attribute(1), attribute(7)) else {
                        continue;
                    };
                    // 2 = PPTP Ethernet UNI, 10 = VEIP.
                    if !matches!(kind, 2 | 10) {
                        continue;
                    }
                    if let Ok(pointer) = u16::try_from(pointer) {
                        evtos.entry(pointer).or_insert(entity.entity_id);
                    }
                }
                _ => {}
            }
        }
        // Prefer the existing assignment on retries; never steal another service's T-CONT.
        let (_, tcont) = tconts.into_iter().min().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "generic bridge has no matching or unassigned T-CONT in MIB",
            )
        })?;
        unis.sort_unstable();
        unis.dedup();
        veips.sort_unstable();
        veips.dedup();
        if unis.is_empty() && veips.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "generic bridge has no Ethernet UNI or VEIP in MIB",
            ));
        }
        u8::try_from(unis.len() + veips.len() + 2).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "too many generic bridge endpoints",
            )
        })?;
        let mut endpoints: Vec<_> = unis
            .into_iter()
            .map(|id| (id, false))
            .chain(veips.into_iter().map(|id| (id, true)))
            .collect();
        // Keep the shipped probe order for existing bridge/EVTO instance assignments.
        endpoints.sort_unstable_by_key(|&(id, veip)| {
            let order: &[u16] = if veip {
                &[0x601, 0x401, 1, 0x8001, 0x101, 0x201]
            } else {
                &[0x101, 0x102, 0x103, 0x104, 1, 2, 3, 4, 0x201, 0x202]
            };
            let index = order.iter().position(|candidate| *candidate == id);
            (index.is_none(), veip, index.unwrap_or(usize::MAX), id)
        });
        used_port_numbers.sort_unstable();
        used_port_numbers.dedup();
        Ok(Self {
            tcont,
            endpoints,
            ports,
            evtos,
            used_port_numbers,
            gem_side,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::mib::{MibAssembler, MibRecord};

    fn inventory(entries: &[(u16, u16, u16)]) -> MibReport {
        let mut mib = MibAssembler::new(u16::try_from(entries.len()).unwrap());
        for (sequence, &(class_id, entity_id, alloc)) in entries.iter().enumerate() {
            let mut data = [0; 26];
            data[..2].copy_from_slice(&alloc.to_be_bytes());
            mib.push(MibRecord {
                sequence: u16::try_from(sequence).unwrap(),
                class_id,
                entity_id,
                attribute_mask: 0x8000,
                data,
            });
        }
        mib.finish()
    }

    #[test]
    fn discovers_star_entities_when_legacy_candidates_are_absent() {
        // Given: the STAR instance layout, including four UNIs and VEIP 0x0501.
        let mib = inventory(&[
            (262, 0x8001, 255),
            (262, 0x8002, 255),
            (11, 0x101, 0),
            (11, 0x102, 0),
            (11, 0x103, 0),
            (11, 0x104, 0),
            (329, 0x501, 0),
        ]);
        // When: planning the bridge for ONU 2.
        let plan = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102]).unwrap();
        // Then: use the advertised instances in their bridge configuration order.
        assert_eq!(
            (plan.tcont, plan.endpoints),
            (
                0x8001,
                vec![
                    (0x101, false),
                    (0x102, false),
                    (0x103, false),
                    (0x104, false),
                    (0x501, true)
                ]
            )
        );
    }

    #[test]
    fn preserves_legacy_endpoint_order_when_new_instances_are_discovered() {
        // Given: old nonascending UNI/VEIP assignments and two newly discoverable endpoints.
        let mib = inventory(&[
            (262, 0x8001, 255),
            (11, 1, 0),
            (11, 0x101, 0),
            (329, 0x601, 0),
            (11, 0x501, 0),
            (329, 0x501, 0),
        ]);
        // When: planning with inventory discovery instead of legacy probes.
        let plan = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102]).unwrap();
        // Then: new instances do not shift the legacy bridge/EVTO assignments.
        assert_eq!(
            plan.endpoints,
            vec![
                (0x101, false),
                (1, false),
                (0x601, true),
                (0x501, false),
                (0x501, true),
            ]
        );
    }

    #[test]
    fn reuses_assignment_when_an_earlier_tcont_is_free() {
        // Given: a free T-CONT before the existing business assignment.
        let mib = inventory(&[(262, 0x8001, 255), (262, 0x8002, 0x102), (329, 0x501, 0)]);
        // When: planning a retry.
        let plan = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102]).unwrap();
        // Then: retain the established allocation.
        assert_eq!(plan.tcont, 0x8002);
    }

    #[test]
    fn foreign_allocation_is_available_but_another_slot_is_not() {
        // Given: one T-CONT from another OLT (Alloc-ID 0x0001) and one owned by slot 3.
        let mib = inventory(&[(262, 0x8001, 0x103), (262, 0x8002, 1), (329, 0x501, 0)]);
        // When: planning for slot 2.
        let plan = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102, 0x103]).unwrap();
        // Then: the foreign allocation is reused, the other slot's is untouched.
        assert_eq!(plan.tcont, 0x8002);
        let mib = inventory(&[(262, 0x8001, 0x103), (329, 0x501, 0)]);
        let result = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102, 0x103]);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_missing_endpoints_instead_of_inventing_a_uni() {
        // Given: no UNI or VEIP in a complete upload.
        let mib = inventory(&[(262, 0x8001, 255)]);
        // When: planning the bridge.
        let result = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102]);
        // Then: configuration stops before writes to a guessed instance.
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_partial_inventory_when_entities_are_missing() {
        // Given: an incomplete upload that happens to contain usable entities.
        let mut mib = inventory(&[(262, 0x8001, 255), (329, 0x501, 0)]);
        mib.uploaded_complete = false;
        // When: planning the bridge.
        let result = BridgeInventory::from_mib(&mib, 0x102, &[0x100, 0x101, 0x102]);
        // Then: absence of other entities is not trusted.
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
}
