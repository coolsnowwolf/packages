//! Shared CWMP mechanisms, independent of optical and bootstrap adapters.
pub mod cache;
pub mod codec;
pub mod compiler;
pub mod contract;
pub mod discovery;
pub mod identity;
pub mod matching;
pub mod observed;
pub mod path;
pub mod probe;
pub mod registry;
pub mod runtime;
pub mod vendor;

pub(crate) mod observations;
pub mod response;
pub mod transport;
