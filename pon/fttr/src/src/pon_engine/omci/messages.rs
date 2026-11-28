use super::{
    error::{invalid, OmciResultError},
    wire::BaselineFrame,
};
use std::io;

/// OMCI-009: action numbers, with AR/AK added only at the wire boundary.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/omcidefs.go#L83-L107>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Action {
    Create = 4,
    Delete = 6,
    Set = 8,
    Get = 9,
    MibUpload = 13,
    MibUploadNext = 14,
    GetNext = 26,
}
impl Action {
    pub fn request_type(self) -> u8 {
        self as u8 | 0x40
    }
}

/// OMCI-002/003/008/010: baseline bodies, excluding class/entity header.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/get.go#L54-L82>
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/mibupload.go>
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/getnext.go#L55-L81>
pub enum RequestBody<'a> {
    Empty,
    Get { mask: u16 },
    Set { mask: u16, data: &'a [u8] },
    Create { data: &'a [u8] },
    UploadNext { sequence: u16 },
    GetNext { mask: u16, sequence: u16 },
}
impl RequestBody<'_> {
    pub fn encode(&self) -> io::Result<[u8; 32]> {
        let mut p = [0; 32];
        match self {
            Self::Empty => (),
            Self::Get { mask } => p[..2].copy_from_slice(&mask.to_be_bytes()),
            Self::Set { mask, data } => {
                if data.len() > 30 {
                    return Err(invalid("baseline SET attributes exceed payload"));
                }
                p[..2].copy_from_slice(&mask.to_be_bytes());
                p[2..2 + data.len()].copy_from_slice(data);
            }
            Self::Create { data } => {
                if data.len() > 32 {
                    return Err(invalid("baseline CREATE attributes exceed payload"));
                }
                p[..data.len()].copy_from_slice(data);
            }
            Self::UploadNext { sequence } => p[..2].copy_from_slice(&sequence.to_be_bytes()),
            Self::GetNext { mask, sequence } => {
                p[..2].copy_from_slice(&mask.to_be_bytes());
                p[2..4].copy_from_slice(&sequence.to_be_bytes());
            }
        }
        Ok(p)
    }
}

/// OMCI-002: GET result=9 has two valid error masks in the last four body octets.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/get.go#L190-L221>
pub struct GetResponse<'a> {
    pub mask: u16,
    pub data: &'a [u8],
}
impl<'a> GetResponse<'a> {
    pub fn decode(frame: &'a BaselineFrame, expected_mask: u16) -> io::Result<Self> {
        let p = &frame.payload;
        let (unsupported_mask, failed_mask) = if p[0] == 9 {
            (
                u16::from_be_bytes([p[28], p[29]]),
                u16::from_be_bytes([p[30], p[31]]),
            )
        } else {
            (0, 0)
        };
        if p[0] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                OmciResultError {
                    result: p[0],
                    unsupported_mask,
                    failed_mask,
                    execution_mask: 0,
                },
            ));
        }
        let mask = u16::from_be_bytes([p[1], p[2]]);
        if mask != expected_mask {
            return Err(invalid("OMCI GET mask mismatch"));
        }
        Ok(Self {
            mask,
            data: &p[3..],
        })
    }
}
pub fn check_result(frame: &BaselineFrame) -> io::Result<()> {
    // OMCI-010: SET masks follow result; CREATE parameter-error carries execution mask.
    // https://github.com/opencord/omci-lib-go/blob/v2.2.4/set.go#L221-L232
    // https://github.com/opencord/omci-lib-go/blob/v2.2.4/create.go#L213-L220
    let p = &frame.payload;
    if p[0] != 0 {
        let set_failure = frame.message_type == 0x28 && p[0] == 9;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            OmciResultError {
                result: p[0],
                unsupported_mask: if set_failure {
                    u16::from_be_bytes([p[1], p[2]])
                } else {
                    0
                },
                failed_mask: if set_failure {
                    u16::from_be_bytes([p[3], p[4]])
                } else {
                    0
                },
                execution_mask: if frame.message_type == 0x24 && p[0] == 3 {
                    u16::from_be_bytes([p[1], p[2]])
                } else {
                    0
                },
            },
        ));
    }
    Ok(())
}
