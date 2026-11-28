use crate::{
    model::{OnuGemPort, OnuOmciInventory, OnuVlanFilter, OnuVlanOperation},
    pon_engine::omci::{
        attributes::AttributeValue,
        mib::{DecodedEntity, MibReport},
    },
};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct InventorySample {
    inventory: OnuOmciInventory,
    at: Instant,
}

impl InventorySample {
    pub fn from_report(report: &MibReport, started: Instant) -> Option<Self> {
        if !report.uploaded_complete {
            return None;
        }
        let mut inventory = OnuOmciInventory {
            age_seconds: 0,
            gem_ports: Vec::new(),
            vlan_filters: Vec::new(),
            vlan_operations: Vec::new(),
        };
        for entity in &report.entities {
            match entity.class_id {
                268 => inventory.gem_ports.push(OnuGemPort {
                    entity_id: entity.entity_id,
                    port_id: unsigned(entity, 1)
                        .and_then(|v| u16::try_from(v).ok())
                        .filter(|v| *v <= 4095),
                }),
                84 => inventory.vlan_filters.push(OnuVlanFilter {
                    entity_id: entity.entity_id,
                    filter_list_hex: match attribute(entity, 1) {
                        Some(AttributeValue::Octets { hex }) => Some(hex.clone()),
                        _ => None,
                    },
                    forward_operation: unsigned(entity, 2),
                    number_of_entries: unsigned(entity, 3),
                }),
                171 => {
                    let table = report.tables.iter().find(|t| {
                        t.class_id == 171
                            && t.entity_id == entity.entity_id
                            && t.attribute.index == 6
                    });
                    inventory.vlan_operations.push(OnuVlanOperation {
                        entity_id: entity.entity_id,
                        received_frame_rules_hex: table
                            .filter(|t| t.collected)
                            .map(|t| t.rows.clone()),
                        error: match table {
                            Some(t) => t.error.clone(),
                            None => Some("vlan_table_not_collected".into()),
                        },
                    });
                }
                _ => {}
            }
        }
        Some(Self {
            inventory,
            at: started,
        })
    }

    pub fn current(&self) -> Option<OnuOmciInventory> {
        self.at.elapsed().lt(&Duration::from_secs(90)).then(|| {
            let mut inventory = self.inventory.clone();
            inventory.age_seconds = self.at.elapsed().as_secs();
            inventory
        })
    }

    pub fn refresh_due(&self, interval: Duration) -> bool {
        self.at.elapsed() >= interval
    }
}

fn attribute(entity: &DecodedEntity, index: u8) -> Option<&AttributeValue> {
    entity
        .attributes
        .iter()
        .find(|a| a.definition.index == index)?
        .value
        .as_ref()
}

fn unsigned(entity: &DecodedEntity, index: u8) -> Option<u32> {
    match attribute(entity, index)? {
        AttributeValue::Unsigned { value } => Some(*value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::mib::{MibAssembler, MibRecord};

    #[test]
    fn port_id_comes_from_attribute_not_entity_id() {
        // Given an actual HWTC-style entity 512 reporting GEM 259.
        let mut assembler = MibAssembler::new(1);
        let mut data = [0; 26];
        data[..2].copy_from_slice(&[1, 3]);
        assembler.push(MibRecord {
            sequence: 0,
            class_id: 268,
            entity_id: 512,
            attribute_mask: 0x8000,
            data,
        });
        // When a complete report becomes a telemetry inventory.
        let sample = InventorySample::from_report(&assembler.finish(), Instant::now()).unwrap();
        // Then the independently reported GEM ID survives.
        assert_eq!(sample.current().unwrap().gem_ports[0].port_id, Some(259));
    }

    #[test]
    fn incomplete_upload_does_not_become_inventory() {
        // Given an upload with a missing record.
        let report = MibAssembler::new(1).finish();
        // When collected, then no complete inventory is claimed.
        assert!(InventorySample::from_report(&report, Instant::now()).is_none());
    }

    #[test]
    fn inventory_expires_independently_of_optical_poll() {
        // Given an upload that began 91 seconds before completion.
        let started = Instant::now() - Duration::from_secs(91);
        let sample = InventorySample::from_report(&MibAssembler::new(0).finish(), started).unwrap();
        // When status reads it, then old configuration is withheld.
        assert!(sample.current().is_none());
    }

    #[test]
    fn segmented_refresh_has_headroom_before_inventory_expiry() {
        let sampled = Instant::now() - Duration::from_secs(35);
        let sample = InventorySample::from_report(&MibAssembler::new(0).finish(), sampled).unwrap();
        assert!(sample.refresh_due(Duration::from_secs(30)));
        assert!(!sample.refresh_due(Duration::from_secs(60)));
        assert!(sample.current().is_some());
    }
}
