use super::{
    attributes::{AttributeCodec, AttributeContext, AttributeValue},
    error::{invalid, Diagnostic},
    schema::{AttributeDefinition, MeRegistry, Reference},
    tables::VlanRule,
    wire::hex,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

/// OMCI-003/007: baseline Upload-NEXT contains one ME header and 26 attribute octets.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/mibupload.go#L397-L446>
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/me.go#L272-L308>
#[derive(Debug, Clone)]
pub struct MibRecord {
    pub sequence: u16,
    pub class_id: u16,
    pub entity_id: u16,
    pub attribute_mask: u16,
    pub data: [u8; 26],
}
impl MibRecord {
    pub fn decode(sequence: u16, payload: &[u8]) -> io::Result<Self> {
        if payload.len() != 32 {
            return Err(invalid("invalid baseline MIB record length"));
        }
        let class_id = u16::from_be_bytes([payload[0], payload[1]]);
        if class_id == 0 {
            return Err(invalid("MIB upload returned empty entity"));
        }
        Ok(Self {
            sequence,
            class_id,
            entity_id: u16::from_be_bytes([payload[2], payload[3]]),
            attribute_mask: u16::from_be_bytes([payload[4], payload[5]]),
            data: payload[6..].try_into().unwrap(),
        })
    }
}

/// Project evidence model: no last-writer-wins merging of conflicting MIB attributes.
#[derive(Debug, serde::Serialize)]
pub struct Observation {
    pub raw_hex: String,
    pub value: AttributeValue,
    pub sequences: Vec<u16>,
}
#[derive(Debug, serde::Serialize)]
pub struct EntityAttribute {
    pub definition: &'static AttributeDefinition,
    pub value: Option<AttributeValue>,
    pub observations: Vec<Observation>,
}
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Layout,
    NameOnly,
    Unknown,
}
#[derive(Debug, serde::Serialize)]
pub struct DecodedEntity {
    pub class_id: u16,
    pub entity_id: u16,
    pub name: Option<&'static str>,
    pub support: Support,
    pub reference: Option<Reference>,
    pub sequences: Vec<u16>,
    pub attributes: Vec<EntityAttribute>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug, serde::Serialize)]
pub struct RecordView {
    pub sequence: u16,
    pub class_id: u16,
    pub entity_id: u16,
    pub attribute_mask: u16,
    pub raw_hex: String,
    pub consumed: usize,
    pub remaining_hex: String,
    pub undecoded_mask: u16,
}
#[derive(Debug, serde::Serialize)]
pub struct ClassSummary {
    pub class_id: u16,
    pub name: Option<&'static str>,
    pub support: Support,
    pub records: usize,
    pub entities: usize,
}

/// Project snapshot format: table data is separate from baseline uploaded attributes.
#[derive(Debug, serde::Serialize)]
pub struct TableSnapshot {
    pub class_id: u16,
    pub entity_id: u16,
    pub attribute: &'static AttributeDefinition,
    pub collected: bool,
    pub error: Option<String>,
    pub raw_hex: String,
    pub rows: Vec<String>,
    pub vlan_rules: Vec<VlanRule>,
}
impl TableSnapshot {
    pub fn from_result(
        class_id: u16,
        entity_id: u16,
        attr: &'static AttributeDefinition,
        result: io::Result<Vec<u8>>,
    ) -> Self {
        let mut table = Self {
            class_id,
            entity_id,
            attribute: attr,
            collected: false,
            error: None,
            raw_hex: String::new(),
            rows: Vec::new(),
            vlan_rules: Vec::new(),
        };
        match result {
            Err(error) => table.error = Some(error.to_string()),
            Ok(data) => {
                table.raw_hex = hex(&data);
                if attr.size == 0 || data.len() % attr.size != 0 {
                    table.error = Some("table snapshot is not a whole number of rows".into());
                } else {
                    for row in data.chunks_exact(attr.size) {
                        table.rows.push(hex(row));
                        if class_id == 171 && attr.index == 6 {
                            table
                                .vlan_rules
                                .push(VlanRule::decode(row).expect("validated legacy row size"));
                        }
                    }
                    table.collected = true;
                }
            }
        }
        table
    }
}

/// Project JSON contract. Upload completeness is independent from schema/table coverage.
#[derive(Debug, serde::Serialize)]
pub struct MibReport {
    pub schema_version: u8,
    pub reference_version: &'static str,
    pub onu_id: Option<u8>,
    pub uploaded_complete: bool,
    pub expected_records: u16,
    pub received_records: usize,
    pub decoded_records: usize,
    pub partial_records: usize,
    pub basic_records: usize,
    pub tables_requested: bool,
    pub tables_collected: bool,
    pub classes: Vec<ClassSummary>,
    pub entities: Vec<DecodedEntity>,
    pub records: Vec<RecordView>,
    pub tables: Vec<TableSnapshot>,
    pub diagnostics: Vec<Diagnostic>,
}

/// OMCI-003 plus project grouping/diagnostics; records are never discarded.
pub struct MibAssembler {
    expected: u16,
    records: Vec<MibRecord>,
}
impl MibAssembler {
    pub fn new(expected: u16) -> Self {
        Self {
            expected,
            records: Vec::new(),
        }
    }
    pub fn push(&mut self, record: MibRecord) {
        self.records.push(record);
    }
    pub fn finish(self) -> MibReport {
        let mut report = MibReport {
            schema_version: 1,
            reference_version: "omci-lib-go/v2.2.4",
            onu_id: None,
            uploaded_complete: false,
            expected_records: self.expected,
            received_records: self.records.len(),
            decoded_records: 0,
            partial_records: 0,
            basic_records: 0,
            tables_requested: false,
            tables_collected: false,
            classes: Vec::new(),
            entities: Vec::new(),
            records: Vec::new(),
            tables: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut sequence_set = BTreeSet::new();
        let mut entities = BTreeMap::new();
        let mut counts = BTreeMap::new();
        for record in self.records {
            if !sequence_set.insert(record.sequence) || record.sequence >= self.expected {
                report.diagnostics.push(Diagnostic::new(
                    "invalid_upload_sequence",
                    format!("duplicate/out-of-range sequence {}", record.sequence),
                    0,
                    record.attribute_mask,
                ));
            }
            *counts.entry(record.class_id).or_insert(0usize) += 1;
            let def = MeRegistry::get(record.class_id);
            let entity = entities
                .entry((record.class_id, record.entity_id))
                .or_insert_with(|| DecodedEntity {
                    class_id: record.class_id,
                    entity_id: record.entity_id,
                    name: def.map(|d| d.name),
                    support: support(record.class_id),
                    reference: def.map(|d| d.reference),
                    sequences: Vec::new(),
                    attributes: Vec::new(),
                    diagnostics: Vec::new(),
                });
            entity.sequences.push(record.sequence);
            let mut view = RecordView {
                sequence: record.sequence,
                class_id: record.class_id,
                entity_id: record.entity_id,
                attribute_mask: record.attribute_mask,
                raw_hex: hex(&record.data),
                consumed: 0,
                remaining_hex: hex(&record.data),
                undecoded_mask: record.attribute_mask,
            };
            if let Some(def) = def.filter(|d| d.layout_supported) {
                let block = AttributeCodec::decode(
                    def,
                    record.attribute_mask,
                    &record.data,
                    AttributeContext::MibUpload,
                );
                if block.undecoded_mask == 0 {
                    report.decoded_records += 1;
                } else {
                    report.partial_records += 1;
                }
                view.consumed = block.consumed;
                view.remaining_hex = block.remaining_hex;
                view.undecoded_mask = block.undecoded_mask;
                entity.diagnostics.extend(block.diagnostics);
                for attr in block.attributes {
                    let entry = if let Some(i) = entity
                        .attributes
                        .iter()
                        .position(|a| a.definition.index == attr.definition.index)
                    {
                        &mut entity.attributes[i]
                    } else {
                        entity.attributes.push(EntityAttribute {
                            definition: attr.definition,
                            value: Some(attr.value.clone()),
                            observations: Vec::new(),
                        });
                        entity.attributes.last_mut().unwrap()
                    };
                    if let Some(prev) = entry
                        .observations
                        .iter_mut()
                        .find(|p| p.raw_hex == attr.raw_hex)
                    {
                        prev.sequences.push(record.sequence);
                    } else {
                        if !entry.observations.is_empty() {
                            entry.value = None;
                            entity.diagnostics.push(Diagnostic::new(
                                "conflicting_attribute",
                                format!("{} has conflicting uploaded values", attr.definition.name),
                                0,
                                attr.definition.mask,
                            ));
                        }
                        entry.observations.push(Observation {
                            raw_hex: attr.raw_hex,
                            value: attr.value,
                            sequences: vec![record.sequence],
                        });
                    }
                }
            } else {
                report.basic_records += 1;
            }
            report.records.push(view);
        }
        report.uploaded_complete = sequence_set.len() == self.expected as usize
            && report.received_records == self.expected as usize
            && sequence_set.iter().copied().eq(0..self.expected);
        if !report.uploaded_complete {
            report.diagnostics.push(Diagnostic::new(
                "incomplete_upload",
                "expected exactly one record for every upload sequence",
                0,
                0,
            ));
        }
        for (_, mut entity) in entities {
            entity.attributes.sort_by_key(|a| a.definition.index);
            report.entities.push(entity);
        }
        for (class_id, records) in counts {
            report.classes.push(ClassSummary {
                class_id,
                name: MeRegistry::get(class_id).map(|d| d.name),
                support: support(class_id),
                records,
                entities: report
                    .entities
                    .iter()
                    .filter(|e| e.class_id == class_id)
                    .count(),
            });
        }
        report
    }
}
fn support(class_id: u16) -> Support {
    match MeRegistry::get(class_id) {
        Some(d) if d.layout_supported => Support::Layout,
        Some(_) => Support::NameOnly,
        None => Support::Unknown,
    }
}
