//! Platform-independent G.988 baseline protocol and transactions.
pub mod attributes;
pub mod error;
pub mod generated;
pub mod messages;
pub mod mib;
pub mod schema;
pub mod session;
pub mod tables;
pub mod wire;

pub use session::OmciSession;
#[cfg(test)]
mod tests;
