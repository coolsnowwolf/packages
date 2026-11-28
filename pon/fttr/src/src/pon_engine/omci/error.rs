use std::{fmt, io};

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// OMCI-002: result-reason is preserved, not inferred from text.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/get.go#L134-L138>
#[derive(Debug)]
pub struct OmciResultError {
    pub result: u8,
    pub unsupported_mask: u16,
    pub failed_mask: u16,
    pub execution_mask: u16,
}
impl fmt::Display for OmciResultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OMCI result={} unsupported_mask={:04x} failed_mask={:04x} execution_mask={:04x}",
            self.result, self.unsupported_mask, self.failed_mask, self.execution_mask
        )
    }
}
impl std::error::Error for OmciResultError {}
/// Result 4 (unknown managed entity) and 5 (unknown instance/attribute) both
/// report "no such entity here". Result 9 is deliberately excluded: shipping
/// ONUs also use it for an entity that exists but whose mask they refuse to
/// report, so it must not be read as "absent".
pub fn is_absent(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<OmciResultError>())
        .is_some_and(|e| matches!(e.result, 4 | 5))
}

/// Raw G.988 result code of an OMCI result error, for caller-side policy.
pub fn result_code(error: &io::Error) -> Option<u8> {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<OmciResultError>())
        .map(|e| e.result)
}

pub fn is_unknown_instance(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<OmciResultError>())
        .is_some_and(|e| e.result == 5)
}

/// Project diagnostic format; offsets refer to the attribute data, not the frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub offset: usize,
    pub mask: u16,
}
impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>, offset: usize, mask: u16) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            offset,
            mask,
        }
    }
}
