use super::error::invalid;
use std::io;

/// OMCI-001: baseline header/body offsets, independent of the transport envelope.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/omci.go#L228-L289>
/// GPON CRC is the deployed legacy integrity profile, not upstream's AES MIC.
#[derive(Debug, Clone, Copy)]
pub struct BaselineFrame {
    pub transaction_id: u16,
    pub message_type: u8,
    pub class_id: u16,
    pub instance_id: u16,
    pub payload: [u8; 32],
}
impl BaselineFrame {
    pub fn new(
        transaction_id: u16,
        message_type: u8,
        class_id: u16,
        instance_id: u16,
        payload: [u8; 32],
    ) -> Self {
        Self {
            transaction_id,
            message_type,
            class_id,
            instance_id,
            payload,
        }
    }
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != 48 || bytes[3] != 0x0a {
            return Err(invalid("unsupported baseline length/device"));
        }
        if bytes[40..44] != [0, 0, 0, 0x28] || bytes[44..48] != crc32(&bytes[..44]).to_be_bytes() {
            return Err(invalid("invalid GPON OMCI trailer/CRC"));
        }
        if u16::from_be_bytes([bytes[0], bytes[1]]) == 0 && bytes[2] & 0x60 != 0 {
            return Err(invalid("zero transaction ID for request/response"));
        }
        Ok(Self::new(
            u16::from_be_bytes([bytes[0], bytes[1]]),
            bytes[2],
            u16::from_be_bytes([bytes[4], bytes[5]]),
            u16::from_be_bytes([bytes[6], bytes[7]]),
            bytes[8..40].try_into().unwrap(),
        ))
    }
    pub fn to_bytes(&self) -> [u8; 48] {
        let mut bytes = [0; 48];
        bytes[..2].copy_from_slice(&self.transaction_id.to_be_bytes());
        bytes[2] = self.message_type;
        bytes[3] = 0x0a;
        bytes[4..6].copy_from_slice(&self.class_id.to_be_bytes());
        bytes[6..8].copy_from_slice(&self.instance_id.to_be_bytes());
        bytes[8..40].copy_from_slice(&self.payload);
        bytes[43] = 0x28;
        let crc = crc32(&bytes[..44]);
        bytes[44..].copy_from_slice(&crc.to_be_bytes());
        bytes
    }
    pub fn answers(&self, request: &Self) -> bool {
        self.transaction_id == request.transaction_id
            && self.message_type == (request.message_type & 0x1f) | 0x20
            && self.class_id == request.class_id
            && self.instance_id == request.instance_id
    }
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x80000000 != 0 {
                (crc << 1) ^ 0x04c11db7
            } else {
                crc << 1
            };
        }
    }
    !crc
}
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}").unwrap();
    }
    text
}
pub fn unhex(text: &str) -> io::Result<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return Err(invalid("invalid hex length/encoding"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| invalid("invalid hex digit")))
        .collect()
}
