//! In-process replies share response ownership; JSON is only a boundary format.
use serde_json::Value;
use std::{
    ops::{Deref, Index},
    sync::Arc,
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Queue {
        device: String,
        method: String,
        args: Value,
    },
    Task {
        id: u64,
    },
    Busy {
        device: String,
    },
    Devices,
    Status,
    Clean,
    ResetDeviceTasks {
        device: String,
    },
}
impl Command {
    pub fn queue(device: &str, method: &str, args: Value) -> Self {
        Self::Queue {
            device: device.into(),
            method: method.into(),
            args,
        }
    }
    #[cfg(test)]
    pub fn wire(self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Clone)]
pub struct Pending<S> {
    pub method: String,
    pub args: Value,
    pub stage: S,
}
pub fn backpressure(reply: &Reply) -> bool {
    reply["code"] == "backpressure"
}

#[derive(Clone)]
pub struct Reply {
    metadata: Value,
    result: Option<Arc<super::response::Data>>,
    // The owner keeps the ACS memory reservation alive while a consumer reads.
    _owner: Option<Arc<dyn Send + Sync>>,
}

impl Reply {
    pub fn shared(
        metadata: Value,
        result: Arc<super::response::Data>,
        owner: Arc<dyn Send + Sync>,
    ) -> Self {
        Self {
            metadata,
            result: Some(result),
            _owner: Some(owner),
        }
    }
    pub fn into_metadata(self) -> Value {
        self.metadata
    }
    pub fn result(&self) -> &super::response::Data {
        static EMPTY: std::sync::OnceLock<super::response::Data> = std::sync::OnceLock::new();
        self.result
            .as_deref()
            .unwrap_or_else(|| EMPTY.get_or_init(Default::default))
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.get(key)
    }
}

impl From<Value> for Reply {
    fn from(mut metadata: Value) -> Self {
        let result = metadata
            .as_object_mut()
            .and_then(|o| o.remove("result"))
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value(v).ok())
            .map(Arc::new);
        Self {
            metadata,
            result,
            _owner: None,
        }
    }
}
impl Deref for Reply {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.metadata
    }
}
impl Index<&str> for Reply {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&Value::Null)
    }
}
