//! HM2004-DU activation resources and ranging evidence.
use crate::{
    platform::h3c_hm2004_du::{fmcs as bosa, omci::leases::update_onu_enable},
    pon_engine::ploam::PloamFrame,
};
use std::{
    io,
    time::{Duration, Instant},
};

pub fn discovery_frames() -> [PloamFrame; 2] {
    use crate::pon_engine::ploam::{PLOAM_EXTENDED_BURST_LENGTH, PLOAM_UPSTREAM_OVERHEAD};
    [
        PloamFrame::new(
            0xff,
            PLOAM_UPSTREAM_OVERHEAD,
            [0x20, 0, 0, 0xaa, 0xab, 0x59, 0x83, 0, 0, 0],
        ),
        PloamFrame::new(
            0xff,
            PLOAM_EXTENDED_BURST_LENGTH,
            [0x40, 0x40, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
    ]
}

pub(crate) struct RangingSession {
    pub id: u8,
    pub serial: [u8; 8],
    pub started: Instant,
    pub armed: bool,
    pub matched: bool,
    pub measured: Option<u32>,
    pub ranged: Option<Instant>,
    pub allocated: bool,
    pub confirmed: bool,
    pub last_valid_rx: Instant,
}
impl RangingSession {
    pub fn observe(&mut self, bytes: &[u8], now: Instant) {
        if bytes.first() == Some(&self.id) && PloamFrame::from_bytes(bytes).is_some() {
            self.last_valid_rx = now;
        }
    }
    pub fn expired(&self, now: Instant) -> bool {
        if self.confirmed {
            now.duration_since(self.last_valid_rx) >= Duration::from_secs(15)
        } else {
            now.duration_since(self.started) >= Duration::from_secs(10)
        }
    }
}

pub(super) fn grant_target(
    active: Option<u8>,
    online: impl Iterator<Item = u8>,
    cycle: usize,
) -> u8 {
    let mut targets = vec![active.unwrap_or(0xfe)];
    targets.extend(online);
    targets[cycle % targets.len()]
}

pub(super) fn clear_offline_hardware(id: u8) -> io::Result<()> {
    // Caller holds this ONU's lock after cancellation and worker lease release.
    crate::platform::h3c_hm2004_du::omci::leases::forget_business_allocation(id);
    bosa::set_fttr_carrier(id as u32, false)?;
    for _ in 0..3 {
        bosa::send_ploam_msg(&PloamFrame::new(id, 5, [0; 10]).to_bytes())?;
    }
    update_onu_enable(id, false)?;
    for address in [
        0x0500009c + 16 * u32::from(id),
        0x050000a4 + 16 * u32::from(id),
    ] {
        bosa::write_fpga_reg(address, 0x01000000)?;
    }
    bosa::exchange_omci_channel(id, 0)?;
    // Do not drain the shared RX queue: it also contains other ONUs' fresh evidence.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_and_activation_preserve_each_online_onu_grant() {
        let targets: Vec<_> = (0..6)
            .map(|cycle| grant_target(None, [1, 2].into_iter(), cycle))
            .collect();
        assert_eq!(targets, [0xfe, 1, 2, 0xfe, 1, 2]);
        let targets: Vec<_> = (0..6)
            .map(|cycle| grant_target(Some(3), [1, 2].into_iter(), cycle))
            .collect();
        assert_eq!(targets, [3, 1, 2, 3, 1, 2]);
        assert_eq!(grant_target(None, [].into_iter(), 99), 0xfe);
    }
    #[test]
    fn anothers_valid_frames_do_not_extend_a_disconnected_onu_lifetime() {
        let start = Instant::now();
        let make = |id| RangingSession {
            id,
            serial: [0; 8],
            started: start,
            armed: true,
            matched: true,
            measured: Some(248248),
            ranged: Some(start),
            allocated: true,
            confirmed: true,
            last_valid_rx: start,
        };
        let mut disconnected = make(1);
        let mut connected = make(2);
        let frame = PloamFrame::new(2, 4, [0; 10]).to_bytes();
        disconnected.observe(&frame, start + Duration::from_secs(14));
        connected.observe(&frame, start + Duration::from_secs(14));
        assert!(disconnected.expired(start + Duration::from_secs(16)));
        assert!(!connected.expired(start + Duration::from_secs(16)));
    }
}
