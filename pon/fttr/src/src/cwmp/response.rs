//! Compact RPC rows: no per-row JSON map or duplicated field names in memory.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ops::{Index, IndexMut},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Row {
    Value {
        name: Value,
        value: Value,
        #[serde(rename = "type", default, skip_serializing_if = "Value::is_null")]
        kind: Value,
    },
    Fault {
        name: Value,
        fault_code: Value,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        fault_string: Value,
    },
    Info {
        name: Value,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        writable: Value,
    },
}
impl Index<&str> for Row {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        match (self, key) {
            (
                Self::Value { name, .. } | Self::Info { name, .. } | Self::Fault { name, .. },
                "name",
            ) => name,
            (Self::Value { value, .. }, "value") => value,
            (Self::Value { kind, .. }, "type") => kind,
            (Self::Info { writable, .. }, "writable") => writable,
            (Self::Fault { fault_code, .. }, "fault_code") => fault_code,
            (Self::Fault { fault_string, .. }, "fault_string") => fault_string,
            _ => &Value::Null,
        }
    }
}
impl Row {
    pub fn heap_bytes(&self) -> usize {
        [
            "name",
            "value",
            "type",
            "writable",
            "fault_code",
            "fault_string",
        ]
        .into_iter()
        .map(|key| match &self[key] {
            Value::String(s) => s.capacity(),
            v => {
                if v.is_null() {
                    0
                } else {
                    memory(v)
                }
            }
        })
        .sum()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Data {
    #[serde(
        default,
        deserialize_with = "bounded_rows",
        skip_serializing_if = "Option::is_none"
    )]
    pub parameters: Option<Vec<Row>>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}
impl Data {
    pub fn rows(&self) -> Option<&[Row]> {
        self.parameters.as_deref()
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.parameters.as_ref().map_or(0, |rows| {
                rows.capacity() * std::mem::size_of::<Row>()
                    + rows.iter().map(Row::heap_bytes).sum::<usize>()
            })
            + self
                .fields
                .iter()
                .map(|(k, v)| k.capacity() + 64 + memory(v))
                .sum::<usize>()
    }
}
impl Index<&str> for Data {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.fields.get(key).unwrap_or(&Value::Null)
    }
}
impl IndexMut<&str> for Data {
    fn index_mut(&mut self, key: &str) -> &mut Value {
        self.fields.entry(key.to_owned()).or_default()
    }
}
pub fn memory(value: &Value) -> usize {
    std::mem::size_of::<Value>()
        + match value {
            Value::String(s) => s.capacity(),
            Value::Array(a) => {
                a.capacity() * std::mem::size_of::<Value>() + a.iter().map(memory).sum::<usize>()
            }
            Value::Object(o) => o.iter().map(|(k, v)| k.capacity() + 64 + memory(v)).sum(),
            _ => 0,
        }
}
fn bounded_rows<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<Row>>, D::Error> {
    struct Rows;
    impl<'de> serde::de::Visitor<'de> for Rows {
        type Value = Option<Vec<Row>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded RPC rows")
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut rows = Vec::new();
            let mut heap = 0usize;
            while let Some(row) = seq.next_element::<Row>()? {
                heap += row.heap_bytes();
                if rows.len() >= 32768
                    || heap + (rows.len() + 1) * std::mem::size_of::<Row>() > 4 * 1024 * 1024
                {
                    return Err(serde::de::Error::custom(
                        "persisted RPC result exceeds budget",
                    ));
                }
                if rows.len() == rows.capacity() {
                    rows.reserve_exact(64);
                }
                rows.push(row);
            }
            Ok(Some(rows))
        }
    }
    d.deserialize_any(Rows)
}
