//! FMCS envelope and capture policy, independent of the G.988 protocol layer.
pub(crate) use crate::platform::openwrt::packet::PacketSocket;
use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
use std::io;

impl PacketSocket {
    pub(crate) fn stop_capture(&self) -> io::Result<()> {
        super::capture::Capture::Closed.attach(self.raw_fd())
    }
    pub(crate) fn health_omci(id: u8) -> io::Result<Self> {
        Self::open_filtered("molt_omci", true, |fd| {
            super::capture::Capture::Omci(id).attach(fd)
        })
    }
    pub(crate) fn health_gem(id: u8) -> io::Result<Self> {
        Self::open_filtered("fttr0", true, |fd| {
            super::capture::Capture::Gem(id).attach(fd)
        })
    }
    pub(crate) fn send_fmcs(&self, id: u8, frame: &BaselineFrame) -> io::Result<()> {
        self.send_broadcast(&envelope(id, frame))
    }
}
pub fn envelope(id: u8, frame: &BaselineFrame) -> [u8; 56] {
    let mut bytes = [0; 56];
    bytes[..4].copy_from_slice(&3u32.to_be_bytes());
    bytes[4..8].copy_from_slice(&(id as u32).to_be_bytes());
    bytes[8..].copy_from_slice(&frame.to_bytes());
    bytes
}
pub fn decode_envelope(bytes: &[u8], kind: u8, id: u8) -> io::Result<Option<BaselineFrame>> {
    if kind == 4
        || bytes.len() != 56
        || bytes[..4] != 3u32.to_be_bytes()
        || bytes[4..8] != (id as u32).to_be_bytes()
    {
        return Ok(None);
    }
    BaselineFrame::from_bytes(&bytes[8..]).map(Some)
}
pub struct FmcsTransport {
    id: u8,
    socket: PacketSocket,
}
impl FmcsTransport {
    pub fn open(id: u8) -> io::Result<Self> {
        Ok(Self {
            id,
            socket: PacketSocket::health_omci(id)?,
        })
    }
}
impl OmciTransport for FmcsTransport {
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        self.socket.send_fmcs(self.id, frame)
    }
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        match self.socket.receive()? {
            Some((bytes, kind, _)) => Ok(decode_envelope(&bytes, kind, self.id).unwrap_or(None)),
            None => Ok(None),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_response_and_onu_echo_isolation() {
        let mut bytes = [0; 56];
        bytes[..8].copy_from_slice(&[0, 0, 0, 3, 0, 0, 0, 1]);
        bytes[8..23].copy_from_slice(&[
            0x65, 0xf0, 0x29, 0x0a, 1, 0, 0, 0, 0, 0x80, 0, 0x48, 0x33, 0x43, 0x54,
        ]);
        bytes[51] = 0x28;
        bytes[52..].copy_from_slice(&[0xd8, 0xcc, 0x94, 0xb4]);
        let request = BaselineFrame::new(0x65f0, 0x49, 256, 0, [0; 32]);
        assert!(decode_envelope(&bytes, 0, 1)
            .unwrap()
            .unwrap()
            .answers(&request));
        assert!(decode_envelope(&bytes, 4, 1).unwrap().is_none());
        assert!(decode_envelope(&bytes, 0, 2).unwrap().is_none());
        bytes[24] ^= 1;
        assert!(decode_envelope(&bytes, 0, 1).is_err());
    }
}
