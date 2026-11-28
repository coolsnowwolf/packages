//! GPON assignable ONU-ID range. Platform capacity is a separate resource policy.
//!
//! 0xfe and 0xff are reserved protocol addresses, not assignable ONU identities.
pub const MIN_ONU_ID: u8 = 0;
pub const MAX_ONU_ID: u8 = 253;

pub const fn is_valid(id: u16) -> bool {
    id <= MAX_ONU_ID as u16
}
