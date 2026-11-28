use std::io;

pub(crate) const MAX_PACKET_BYTES: usize = 3 * 1024 * 1024;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "MQTT packet length is malformed or exceeds 3145728 bytes",
    )
}

pub(crate) fn publish_size(topic_bytes: usize, payload_bytes: usize) -> io::Result<usize> {
    if topic_bytes > usize::from(u16::MAX) {
        return Err(invalid());
    }
    let remaining = topic_bytes
        .checked_add(payload_bytes)
        .and_then(|n| n.checked_add(2))
        .ok_or_else(invalid)?;
    let mut value = remaining;
    let mut header = 2;
    while value >= 128 {
        header += 1;
        value /= 128;
    }
    let total = remaining.checked_add(header).ok_or_else(invalid)?;
    if total > MAX_PACKET_BYTES {
        return Err(invalid());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "mqtt")]
    #[test]
    fn mqtt_command_packet_header_rejects_oversize_and_malformed_lengths() {
        use rumqttc::mqttbytes::{check, Error};
        let limit = crate::mqtt::session::MAX_REMAINING_BYTES;
        assert!(matches!(
            check([0x30, 0xfb, 0xff, 0xbf, 1].iter(), limit),
            Err(Error::InsufficientBytes(_))
        ));
        assert!(matches!(
            check([0x30, 0xfc, 0xff, 0xbf, 1].iter(), limit),
            Err(Error::PayloadSizeLimitExceeded(_))
        ));
        assert!(matches!(
            check([0x30, 0xff, 0xff, 0xff, 0x80].iter(), limit),
            Err(Error::MalformedRemainingLength)
        ));
        assert!(matches!(
            check([0x30, 0x80].iter(), limit),
            Err(Error::InsufficientBytes(_))
        ));
        assert!(matches!(
            check([0x30, 0x80, 1].iter(), limit),
            Err(Error::InsufficientBytes(_))
        ));
    }

    #[test]
    fn mqtt_command_publish_limit_counts_the_entire_wire_packet() {
        assert_eq!(publish_size(1, 3_145_720).unwrap(), 3_145_728);
        assert!(publish_size(1, 3_145_721).is_err());
        assert!(publish_size(65_536, 0).is_err());
    }
}
