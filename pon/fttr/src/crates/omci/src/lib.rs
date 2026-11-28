//! Pure protocol objects. Hardware, registration and device policy belong to callers.
#[path = "../../../src/pon_engine/omci/attributes.rs"]
pub mod attributes;
#[path = "../../../src/pon_engine/omci/error.rs"]
pub mod error;
#[path = "../../../src/pon_engine/omci/generated/mod.rs"]
pub mod generated;
#[path = "../../../src/pon_engine/omci/messages.rs"]
pub mod messages;
#[path = "../../../src/pon_engine/omci/mib.rs"]
pub mod mib;
#[path = "../../../src/pon_engine/omci/schema.rs"]
pub mod schema;
#[path = "../../../src/pon_engine/omci/session.rs"]
pub mod session;
#[path = "../../../src/pon_engine/omci/tables.rs"]
pub mod tables;
#[cfg(test)]
#[path = "../../../src/pon_engine/omci/tests.rs"]
mod tests;
#[path = "../../../src/pon_engine/omci/wire.rs"]
pub mod wire;
