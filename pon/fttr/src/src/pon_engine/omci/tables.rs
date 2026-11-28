use super::{error::invalid, wire::hex};
use std::io;

/// OMCI-008: ordered GET-NEXT fragments, trimmed to the length established by GET.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/getnext.go#L160-L209>
/// Snapshot bounds and sequence validation are project orchestration.
pub struct TableAssembler {
    length: usize,
    next: u16,
    data: Vec<u8>,
}
impl TableAssembler {
    pub fn new(length: usize) -> io::Result<Self> {
        if length > 4096 {
            return Err(invalid("OMCI table exceeds diagnostic bound"));
        }
        Ok(Self {
            length,
            next: 0,
            data: Vec::with_capacity(length),
        })
    }
    pub fn segments(&self) -> usize {
        self.length.div_ceil(29)
    }
    pub fn push(&mut self, sequence: u16, part: &[u8]) -> io::Result<()> {
        if sequence != self.next || self.data.len() == self.length {
            return Err(invalid("duplicate/out-of-order table segment"));
        }
        let count = (self.length - self.data.len()).min(29);
        if part.len() < count {
            return Err(invalid("truncated GET NEXT fragment"));
        }
        self.data.extend_from_slice(&part[..count]);
        self.next += 1;
        Ok(())
    }
    pub fn finish(self) -> io::Result<Vec<u8>> {
        if self.data.len() != self.length {
            return Err(invalid("incomplete table snapshot"));
        }
        Ok(self.data)
    }
}

/// ME-00171: legacy 16-octet VLAN rule fields, without interpreting reserved bits.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/extendedvlantaggingoperationconfigurationdata.go#L183-L380>
#[derive(Debug, serde::Serialize)]
pub struct VlanRule {
    pub raw_hex: String,
    pub filter_outer_priority: u8,
    pub filter_outer_vid: u16,
    pub filter_outer_tpid_dei: u8,
    pub filter_inner_priority: u8,
    pub filter_inner_vid: u16,
    pub filter_inner_tpid_dei: u8,
    pub filter_ethertype: u8,
    pub tags_to_remove: u8,
    pub treatment_outer_priority: u8,
    pub treatment_outer_vid: u16,
    pub treatment_outer_tpid_dei: u8,
    pub treatment_inner_priority: u8,
    pub treatment_inner_vid: u16,
    pub treatment_inner_tpid_dei: u8,
    pub delete_entry: bool,
}
impl VlanRule {
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != 16 {
            return Err(invalid("legacy VLAN rule must be 16 octets"));
        }
        let w: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
            .collect();
        Ok(Self {
            raw_hex: hex(bytes),
            filter_outer_priority: (w[0] >> 28) as u8,
            filter_outer_vid: ((w[0] >> 15) & 0x1fff) as u16,
            filter_outer_tpid_dei: ((w[0] >> 12) & 7) as u8,
            filter_inner_priority: (w[1] >> 28) as u8,
            filter_inner_vid: ((w[1] >> 15) & 0x1fff) as u16,
            filter_inner_tpid_dei: ((w[1] >> 12) & 7) as u8,
            filter_ethertype: (w[1] & 15) as u8,
            tags_to_remove: (w[2] >> 30) as u8,
            treatment_outer_priority: ((w[2] >> 16) & 15) as u8,
            treatment_outer_vid: ((w[2] >> 3) & 0x1fff) as u16,
            treatment_outer_tpid_dei: (w[2] & 7) as u8,
            treatment_inner_priority: ((w[3] >> 16) & 15) as u8,
            treatment_inner_vid: ((w[3] >> 3) & 0x1fff) as u16,
            treatment_inner_tpid_dei: (w[3] & 7) as u8,
            delete_entry: bytes[8..].iter().all(|b| *b == 0xff),
        })
    }
}
