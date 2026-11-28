use super::{
    error::{invalid, Diagnostic},
    schema::{AttributeDefinition, AttributeKind, MeDefinition},
    wire::{hex, unhex},
};
use std::{collections::BTreeMap, io};

/// Message context is mandatory: table lengths and rows are not interchangeable.
/// OMCI-004 <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/medef.go#L117-L155>
#[derive(Debug, Clone, Copy)]
pub enum AttributeContext {
    MibUpload,
    GetResponse,
    SetRequest,
    CreateRequest,
}

/// Project JSON view: all 64-bit integers are decimal strings, avoiding JS rounding.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttributeValue {
    Unsigned { value: u32 },
    Signed { value: i32 },
    Decimal { value: String },
    Text { value: Option<String> },
    Octets { hex: String },
    TableLength { bytes: u32 },
}
impl AttributeValue {
    pub fn octets(bytes: &[u8]) -> Self {
        Self::Octets { hex: hex(bytes) }
    }
    pub fn number(value: u32) -> Self {
        Self::Unsigned { value }
    }
    pub fn text(&self) -> Option<&str> {
        if let Self::Text { value } = self {
            value.as_deref()
        } else {
            None
        }
    }
    pub fn unsigned(&self) -> Option<u32> {
        if let Self::Unsigned { value } = self {
            Some(*value)
        } else {
            None
        }
    }
}

/// A decoded attribute always retains exact octets and its authoritative definition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecodedAttribute {
    pub definition: &'static AttributeDefinition,
    pub raw_hex: String,
    pub value: AttributeValue,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttributeBlock {
    pub attributes: Vec<DecodedAttribute>,
    pub diagnostics: Vec<Diagnostic>,
    pub consumed: usize,
    pub remaining_hex: String,
    pub undecoded_mask: u16,
}

/// OMCI-004/005: ordered mask walk and big-endian primitive encoding.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/medef.go#L89-L168>
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/attribute.go#L136-L214>
pub struct AttributeCodec;
impl AttributeCodec {
    pub fn read_mask(def: &MeDefinition, indices: &[u8]) -> io::Result<u16> {
        if !def.layout_supported || !def.actions.contains(&9) {
            return Err(invalid("ME does not support schema GET"));
        }
        let mut mask = 0;
        let mut size = 0;
        for &index in indices {
            let attr = def
                .attribute(index)
                .filter(|a| a.index != 0 && a.read)
                .ok_or_else(|| invalid("unknown/unreadable attribute"))?;
            if mask & attr.mask != 0 {
                return Err(invalid("duplicate read attribute"));
            }
            mask |= attr.mask;
            size += if attr.kind == AttributeKind::Table {
                4
            } else {
                attr.size
            };
        }
        if mask == 0 || size > 29 {
            return Err(invalid("empty/oversized baseline GET"));
        }
        Ok(mask)
    }
    pub fn decode(
        def: &'static MeDefinition,
        mask: u16,
        data: &[u8],
        context: AttributeContext,
    ) -> AttributeBlock {
        let mut block = AttributeBlock {
            attributes: Vec::new(),
            diagnostics: Vec::new(),
            consumed: 0,
            remaining_hex: hex(data),
            undecoded_mask: mask,
        };
        for index in 1..=16 {
            let bit = 1 << (16 - index);
            if mask & bit == 0 {
                continue;
            }
            let Some(attr) = def.attribute(index) else {
                block.diagnostics.push(Diagnostic::new(
                    "unknown_attribute",
                    "unknown wire width; subsequent offsets cannot be trusted",
                    block.consumed,
                    bit,
                ));
                break;
            };
            let size = if attr.kind == AttributeKind::Table {
                if matches!(context, AttributeContext::GetResponse) {
                    4
                } else if matches!(context, AttributeContext::SetRequest) {
                    attr.size
                } else {
                    block.diagnostics.push(Diagnostic::new(
                        "table_context_unsupported",
                        "MIB upload does not carry a GET table snapshot",
                        block.consumed,
                        bit,
                    ));
                    break;
                }
            } else {
                attr.size
            };
            if size == 0 || data.len().saturating_sub(block.consumed) < size {
                block.diagnostics.push(Diagnostic::new(
                    "truncated_attribute",
                    format!("{} requires {size} octets", attr.name),
                    block.consumed,
                    bit,
                ));
                break;
            }
            let raw = &data[block.consumed..block.consumed + size];
            let value = if attr.kind == AttributeKind::Table
                && matches!(context, AttributeContext::GetResponse)
            {
                AttributeValue::TableLength {
                    bytes: u32::from_be_bytes(raw.try_into().unwrap()),
                }
            } else {
                Self::decode_value(attr, raw)
            };
            block.attributes.push(DecodedAttribute {
                definition: attr,
                raw_hex: hex(raw),
                value,
            });
            block.consumed += size;
            block.undecoded_mask &= !bit;
        }
        block.remaining_hex = hex(&data[block.consumed..]);
        block
    }
    fn decode_value(attr: &AttributeDefinition, raw: &[u8]) -> AttributeValue {
        match attr.kind {
            AttributeKind::String => {
                let end = raw
                    .iter()
                    .position(|b| *b == 0 || *b == 0xff)
                    .unwrap_or(raw.len());
                let value = std::str::from_utf8(&raw[..end])
                    .ok()
                    .filter(|s| s.bytes().all(|b| b.is_ascii_graphic() || b == b' '))
                    .map(|s| s.trim_end().to_string());
                AttributeValue::Text { value }
            }
            AttributeKind::Unknown | AttributeKind::Octets | AttributeKind::Table => {
                AttributeValue::Octets { hex: hex(raw) }
            }
            AttributeKind::SignedInteger => match raw.len() {
                1 => AttributeValue::Signed {
                    value: raw[0] as i8 as i32,
                },
                2 => AttributeValue::Signed {
                    value: i16::from_be_bytes(raw.try_into().unwrap()) as i32,
                },
                4 => AttributeValue::Signed {
                    value: i32::from_be_bytes(raw.try_into().unwrap()),
                },
                8 => AttributeValue::Decimal {
                    value: i64::from_be_bytes(raw.try_into().unwrap()).to_string(),
                },
                _ => AttributeValue::Octets { hex: hex(raw) },
            },
            _ => match raw.len() {
                1 | 2 | 4 => AttributeValue::Unsigned {
                    value: raw.iter().fold(0, |v, b| (v << 8) | *b as u32),
                },
                8 => AttributeValue::Decimal {
                    value: u64::from_be_bytes(raw.try_into().unwrap()).to_string(),
                },
                _ => AttributeValue::Octets { hex: hex(raw) },
            },
        }
    }
    pub fn encode(
        def: &MeDefinition,
        values: &[(u8, AttributeValue)],
        context: AttributeContext,
    ) -> io::Result<(u16, Vec<u8>)> {
        let action = match context {
            AttributeContext::SetRequest => 8,
            AttributeContext::CreateRequest => 4,
            _ => return Err(invalid("encoding requires request context")),
        };
        if !def.layout_supported || !def.actions.contains(&action) {
            return Err(invalid("ME does not support schema request"));
        }
        if action == 8 && values.is_empty() {
            return Err(invalid("empty SET"));
        }
        let mut inputs = BTreeMap::new();
        for (index, value) in values {
            if *index == 0 || inputs.insert(*index, value).is_some() {
                return Err(invalid("invalid/duplicate attribute index"));
            }
            let attr = def
                .attribute(*index)
                .ok_or_else(|| invalid("unknown attribute"))?;
            let allowed = match context {
                AttributeContext::SetRequest => attr.write,
                AttributeContext::CreateRequest => attr.set_by_create,
                _ => false,
            };
            if !allowed {
                return Err(invalid(format!(
                    "{} access does not permit this request",
                    attr.name
                )));
            }
        }
        let mut mask = 0;
        let mut bytes = Vec::new();
        for attr in def.attributes.iter().filter(|a| a.index != 0) {
            if let Some(value) = inputs.get(&attr.index) {
                bytes.extend(Self::encode_value(attr, value)?);
                mask |= attr.mask;
            } else if matches!(context, AttributeContext::CreateRequest) && attr.set_by_create {
                let default = attr
                    .default_hex
                    .ok_or_else(|| invalid(format!("missing create attribute {}", attr.name)))?;
                bytes.extend(unhex(default)?);
                mask |= attr.mask;
            }
        }
        let capacity = if matches!(context, AttributeContext::CreateRequest) {
            32
        } else {
            30
        };
        if bytes.len() > capacity {
            return Err(invalid("encoded attributes exceed baseline capacity"));
        }
        Ok((mask, bytes))
    }
    pub(super) fn encode_value(
        attr: &AttributeDefinition,
        value: &AttributeValue,
    ) -> io::Result<Vec<u8>> {
        let numeric = !matches!(
            attr.kind,
            AttributeKind::String
                | AttributeKind::Octets
                | AttributeKind::Unknown
                | AttributeKind::Table
        );
        let bytes = match value {
            AttributeValue::Unsigned { value }
                if numeric
                    && attr.kind != AttributeKind::SignedInteger
                    && [1, 2, 4].contains(&attr.size) =>
            {
                if attr.size < 4 && *value >= (1u32 << (attr.size * 8)) {
                    return Err(invalid("attribute integer out of range"));
                }
                value.to_be_bytes()[4 - attr.size..].to_vec()
            }
            AttributeValue::Signed { value }
                if attr.kind == AttributeKind::SignedInteger && [1, 2, 4].contains(&attr.size) =>
            {
                let shift = 32 - attr.size * 8;
                if (*value << shift) >> shift != *value {
                    return Err(invalid("signed attribute out of range"));
                }
                value.to_be_bytes()[4 - attr.size..].to_vec()
            }
            AttributeValue::Decimal { value } if numeric && attr.size == 8 => {
                if attr.kind == AttributeKind::SignedInteger {
                    value
                        .parse::<i64>()
                        .map_err(|_| invalid("invalid signed decimal"))?
                        .to_be_bytes()
                        .to_vec()
                } else {
                    value
                        .parse::<u64>()
                        .map_err(|_| invalid("invalid unsigned decimal"))?
                        .to_be_bytes()
                        .to_vec()
                }
            }
            AttributeValue::Text { value: Some(value) } if attr.kind == AttributeKind::String => {
                if !value.is_ascii() || value.len() > attr.size {
                    return Err(invalid("invalid fixed-width string"));
                }
                let mut bytes = value.as_bytes().to_vec();
                bytes.resize(attr.size, 0);
                bytes
            }
            AttributeValue::Octets { hex } if !numeric => unhex(hex)?,
            _ => return Err(invalid("attribute value/type mismatch")),
        };
        if bytes.len() != attr.size {
            return Err(invalid("attribute width mismatch"));
        }
        Ok(bytes)
    }
}
