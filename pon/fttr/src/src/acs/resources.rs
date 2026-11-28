//! Reservations follow response ownership, including consumers outside State.
use crate::cwmp::response::Data;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub(super) const RESULT_BYTES: usize = 4 * 1024 * 1024;
pub(super) const SMALL_LANE: usize = 256 * 1024;
pub(super) const ARGUMENT_BYTES: usize = 256 * 1024;
thread_local! { static RESTORE: std::cell::RefCell<Option<Arc<Budget>>> = const { std::cell::RefCell::new(None) }; }
pub(super) fn restoring<T>(budget: Arc<Budget>, read: impl FnOnce() -> T) -> T {
    struct Scope(Option<Arc<Budget>>);
    impl Drop for Scope {
        fn drop(&mut self) {
            RESTORE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _scope = Scope(RESTORE.with(|slot| slot.replace(Some(budget))));
    read()
}

#[derive(Default)]
pub(super) struct Budget {
    used: AtomicUsize,
}
impl Budget {
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize, ceiling: usize) -> Option<Permit> {
        let mut used = self.used();
        loop {
            let next = used.checked_add(bytes).filter(|next| *next <= ceiling)?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
        Some(Permit(Arc::new(Charge {
            budget: self.clone(),
            bytes: AtomicUsize::new(bytes),
        })))
    }
}
struct Charge {
    budget: Arc<Budget>,
    bytes: AtomicUsize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget
            .used
            .fetch_sub(self.bytes.load(Ordering::Acquire), Ordering::AcqRel);
        super::connection_request::wake();
        crate::provisioning::controller::wake();
    }
}
#[derive(Clone)]
pub(super) struct Permit(Arc<Charge>);
impl Permit {
    pub fn bytes(&self) -> usize {
        self.0.bytes.load(Ordering::Acquire)
    }
    pub fn shrink(self, bytes: usize) -> Option<Self> {
        let old = self.bytes();
        if bytes > old {
            return None;
        }
        self.0.bytes.store(bytes, Ordering::Release);
        self.0.budget.used.fetch_sub(old - bytes, Ordering::AcqRel);
        if bytes < old {
            super::connection_request::wake();
            crate::provisioning::controller::wake();
        }
        Some(self)
    }
}

#[derive(Clone)]
pub(super) struct Response(Arc<Payload>);
struct Payload {
    value: Arc<Data>,
    _permit: Option<Permit>,
    bytes: usize,
}
impl Response {
    pub fn charged_already(&self) -> bool {
        self.0._permit.is_some()
    }
    pub fn with_charge(self, permit: Permit) -> Self {
        Self(Arc::new(Payload {
            value: self.0.value.clone(),
            bytes: self.0.bytes,
            _permit: Some(permit),
        }))
    }
    pub fn uncharged(value: Value) -> Self {
        Self::from_data(serde_json::from_value(value).expect("valid RPC result"))
    }
    fn from_data(value: Data) -> Self {
        let bytes = value.bytes();
        Self(Arc::new(Payload {
            value: Arc::new(value),
            _permit: None,
            bytes,
        }))
    }
    pub fn charged(value: Data, permit: Permit) -> Self {
        let bytes = value.bytes();
        Self(Arc::new(Payload {
            value: Arc::new(value),
            _permit: Some(permit),
            bytes,
        }))
    }
    pub fn get(&self) -> &Data {
        &self.0.value
    }
    pub fn bytes(&self) -> usize {
        self.0.bytes
    }
    pub fn reply(&self, metadata: Value) -> crate::cwmp::transport::Reply {
        crate::cwmp::transport::Reply::shared(metadata, self.0.value.clone(), self.0.clone())
    }
}
impl std::ops::Deref for Response {
    type Target = Data;
    fn deref(&self) -> &Data {
        self.get()
    }
}
impl Serialize for Response {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(s)
    }
}
impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Data::deserialize(d)?;
        RESTORE.with(|slot| match slot.borrow().as_ref() {
            Some(budget) => budget
                .reserve(value.bytes(), RESULT_BYTES)
                .map(|permit| Self::charged(value, permit))
                .ok_or_else(|| serde::de::Error::custom("restored responses exceed memory budget")),
            None => Ok(Self::from_data(value)),
        })
    }
}

pub(super) use crate::cwmp::response::memory;
pub(super) fn reservation(method: &str) -> usize {
    match method {
        "GetParameterNames" => 2 * 1024 * 1024,
        "GetParameterValues" => 512 * 1024,
        _ => 64 * 1024,
    }
}
