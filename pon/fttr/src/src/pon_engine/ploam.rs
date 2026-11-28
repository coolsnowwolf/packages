//! ITU-T G.984.3 GPON PLOAM (Physical Layer OAM) Protocol Engine
//!
//! Handles ONU discovery, activation state machine (O1 -> O5), ranging,
//! and equalization delay calculation for all downstream ONUs.

pub use fttr_api::RegistrationState as OnuState;

#[cfg(test)]
mod state_tests {
    use super::OnuState;

    #[test]
    fn onu_state_wire_round_trip_uses_protocol_phase_names() {
        for (state, wire) in [
            (OnuState::O1Initial, "o1_initial"),
            (OnuState::O2Standby, "o2_standby"),
            (OnuState::O3SerialNumber, "o3_serial_number"),
            (OnuState::O4Ranging, "o4_ranging"),
            (OnuState::O5Operation, "o5_operation"),
            (OnuState::O6IntermittentLODS, "o6_intermittent_lods"),
            (OnuState::O7EmergencyStop, "o7_emergency_stop"),
        ] {
            let json = serde_json::to_value(state).unwrap();
            assert_eq!(json, wire);
            assert_eq!(serde_json::from_value::<OnuState>(json).unwrap(), state);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GponOnu {
    pub onu_id: u8,
    pub vendor_id: [u8; 4],
    pub serial_number: [u8; 4],
    pub state: OnuState,
    pub eq_delay: Option<u32>,
    pub pon_mode: Option<String>,
    pub upstream_phy_rate_gbps: Option<f32>,
    pub model: String,
    pub firmware_version: String,
    pub hardware_version: String,
}

impl GponOnu {
    pub fn new(onu_id: u8, vendor_id: [u8; 4], serial_number: [u8; 4]) -> Self {
        // Equipment identity and versions require ONU OMCI responses.
        let model = "Unknown".to_string();
        let hardware_version = "Unknown".to_string();
        let firmware_version = "Unknown".to_string();

        Self {
            onu_id,
            vendor_id,
            serial_number,
            state: OnuState::O1Initial,
            eq_delay: None,
            pon_mode: None,
            upstream_phy_rate_gbps: None,
            model,
            firmware_version,
            hardware_version,
        }
    }

    pub fn vendor_str(&self) -> String {
        String::from_utf8_lossy(&self.vendor_id).to_string()
    }

    pub fn sn_hex(&self) -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}",
            self.serial_number[0],
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3]
        )
    }

    pub fn full_sn_str(&self) -> String {
        format!("{}{}", self.vendor_str(), self.sn_hex())
    }
}

// ITU-T G.984.3 Standard Downstream PLOAM Message IDs
pub const PLOAM_UPSTREAM_OVERHEAD: u8 = 0x01;
pub const PLOAM_SERIAL_NUMBER_MASK: u8 = 0x0C;
pub const PLOAM_ASSIGN_ONU_ID: u8 = 0x03;
pub const PLOAM_ASSIGN_DELAY: u8 = 0x04;
pub const PLOAM_DEACTIVATE_ONU_ID: u8 = 0x05;
pub const PLOAM_DISABLE_SERIAL_NUMBER: u8 = 0x06;
pub const PLOAM_EXTENDED_BURST_LENGTH: u8 = 0x14;
pub const PLOAM_ENCRYPTED_PORT_ID: u8 = 0x08;
pub const PLOAM_REQUEST_PASSWORD: u8 = 0x09;

// ITU-T G.984.3 Standard Upstream PLOAM Message IDs
pub const PLOAM_UPSTREAM_SERIAL_NUMBER: u8 = 0x01;
pub const PLOAM_UPSTREAM_PASSWORD: u8 = 0x02;
pub const PLOAM_UPSTREAM_DYING_GASP: u8 = 0x03;
pub const PLOAM_UPSTREAM_ACK: u8 = 0x09;

/// Standard 13-byte G.984.3 Downstream PLOAM message format
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct PloamFrame {
    pub onu_id: u8,        // 0x00-0xFF (0xFF = broadcast)
    pub message_id: u8,    // PLOAM message ID
    pub payload: [u8; 10], // 10 bytes parameter payload
    pub crc: u8,           // CRC-8 check
}

impl PloamFrame {
    /// The post-ranging type-1 Alloc-ID equals the assigned ONU-ID.
    pub fn assign_alloc_id(onu_id: u8) -> Self {
        let mut payload = [0; 10];
        payload[0] = 1;
        payload[1] = onu_id >> 4;
        payload[2] = onu_id << 4;
        Self::new(onu_id, 0x0e, payload)
    }

    pub fn new(onu_id: u8, message_id: u8, payload: [u8; 10]) -> Self {
        let mut frame = Self {
            onu_id,
            message_id,
            payload,
            crc: 0,
        };
        frame.crc = frame.compute_crc8();
        frame
    }

    pub fn compute_crc8(&self) -> u8 {
        let mut crc = 0u8;
        let bytes = [
            self.onu_id,
            self.message_id,
            self.payload[0],
            self.payload[1],
            self.payload[2],
            self.payload[3],
            self.payload[4],
            self.payload[5],
            self.payload[6],
            self.payload[7],
            self.payload[8],
            self.payload[9],
        ];

        for &b in &bytes {
            crc ^= b;
            for _ in 0..8 {
                if (crc & 0x80) != 0 {
                    crc = (crc << 1) ^ 0x07;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    }

    pub fn to_bytes(&self) -> [u8; 13] {
        let mut out = [0u8; 13];
        out[0] = self.onu_id;
        out[1] = self.message_id;
        out[2..12].copy_from_slice(&self.payload);
        out[12] = self.crc;
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 13 {
            return None;
        }
        let mut payload = [0; 10];
        payload.copy_from_slice(&bytes[2..12]);
        let frame = Self::new(bytes[0], bytes[1], payload);
        if frame.crc == bytes[12] {
            Some(frame)
        } else {
            None
        }
    }

    /// 2. Serial Number Mask message (broadcast to trigger ONU discovery)
    pub fn serial_number_mask() -> Self {
        // Mask length 0 triggers wildcard response from all undiscovered ONUs
        let payload = [0u8; 10];
        Self::new(0xFF, PLOAM_SERIAL_NUMBER_MASK, payload)
    }

    /// 3. Assign ONU-ID message
    pub fn assign_onu_id(onu_id: u8, vendor: [u8; 4], sn: [u8; 4]) -> Self {
        let mut payload = [0u8; 10];
        payload[0] = onu_id;
        payload[1..5].copy_from_slice(&vendor);
        payload[5..9].copy_from_slice(&sn);
        Self::new(0xFF, PLOAM_ASSIGN_ONU_ID, payload)
    }

    /// 5. Assign Delay (EqD) message to transition ONU to O5 (Operation State)
    pub fn assign_delay(onu_id: u8, delay_bits: u32) -> Self {
        let mut payload = [0u8; 10];
        payload[0] = 0; // Main path, absolute equalization delay.
        payload[1..5].copy_from_slice(&delay_bits.to_be_bytes());
        Self::new(onu_id, PLOAM_ASSIGN_DELAY, payload)
    }
}
