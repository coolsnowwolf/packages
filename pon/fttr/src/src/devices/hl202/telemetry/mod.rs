mod archive;
mod decode;
pub mod store;
mod worker;
pub use fttr_api::{HlTelemetry as Report, HlTelemetryField as Field, HlTelemetryMember as Member};

pub fn parse(input: &[u8]) -> std::io::Result<Report> {
    parse_measured(input).map(|(report, _)| report)
}

fn parse_measured(input: &[u8]) -> std::io::Result<(Report, usize)> {
    let (members, decompressed_bytes) = archive::read(input)?;
    let report = decode::report(members, input.len(), decompressed_bytes);
    let bytes = report_bytes(&report)?;
    Ok((report, bytes))
}

/// Identity claims are only candidates until independently correlated with the
/// current optical registration and bridge source port.
pub(crate) fn identity_claim(report: &Report, topic_mac: &str) -> Option<(String, String)> {
    let member = |name: &str| {
        report
            .members
            .iter()
            .find(|m| m.name == name)?
            .text
            .as_deref()
            .map(str::trim)
    };
    let serial = member("SN")?;
    if serial.len() != 12 || !serial.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let mac = crate::mqtt_identity::normalize_target(member("sn")?)?;
    if crate::mqtt_identity::normalize_target(topic_mac)? != mac
        || u8::from_str_radix(&mac[..2], 16).ok()? & 1 != 0
        || mac == "000000000000"
    {
        return None;
    }
    Some((serial.to_ascii_uppercase(), mac))
}

fn report_bytes(report: &Report) -> std::io::Result<usize> {
    struct ByteCount(usize);
    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = ByteCount(0);
    serde_json::to_writer(&mut encoded, report).map_err(std::io::Error::other)?;
    if encoded.0 > 4 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "telemetry JSON limit exceeded",
        ));
    }
    Ok(encoded.0)
}

#[cfg(test)]
mod tests;
