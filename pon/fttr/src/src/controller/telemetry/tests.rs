use super::*;
use crate::pon_engine::omci::{
    attributes::{AttributeCodec, AttributeContext},
    schema::MeRegistry,
};
use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};

struct GetTransport {
    request: Option<BaselineFrame>,
    result: u8,
    octets: [u8; 2],
}

impl OmciTransport for GetTransport {
    fn send(&mut self, request: &BaselineFrame) -> std::io::Result<()> {
        assert_eq!(
            (request.message_type, request.class_id, request.instance_id),
            (0x49, 263, 0x8001)
        );
        self.request = Some(request.clone());
        Ok(())
    }

    fn receive(&mut self) -> std::io::Result<Option<BaselineFrame>> {
        Ok(self.request.take().map(|request| {
            let mut payload = [0; 32];
            payload[..5].copy_from_slice(&[self.result, 0, 0x40, self.octets[0], self.octets[1]]);
            BaselineFrame::new(request.transaction_id, 0x29, 263, 0x8001, payload)
        }))
    }
}

#[test]
fn measurement_with_raw_evidence_when_omci_get_succeeds() {
    let mut session = OmciSession::new(GetTransport {
        request: None,
        result: 0,
        octets: [0xe7, 0x96],
    });
    let (value, evidence) = read(&mut session, (263, 0x8001, 0x0040), true);
    assert_eq!(value, Some(-12.5));
    assert_eq!(evidence.raw.as_deref(), Some("e796"));
    assert_eq!(evidence.reason, None);
}

#[test]
fn unavailable_with_protocol_reason_when_omci_get_is_unsupported() {
    let mut session = OmciSession::new(GetTransport {
        request: None,
        result: 4,
        octets: [0, 0],
    });
    let (value, evidence) = read(&mut session, (263, 0x8001, 0x0040), true);
    assert_eq!(value, None);
    assert!(evidence.reason.unwrap().contains("result=4"));
    assert_eq!(evidence.raw, None);
}

#[test]
fn measurement_respects_expired_outer_deadline() {
    let mut session = OmciSession::new(GetTransport {
        request: None,
        result: 0,
        octets: [0, 0],
    });
    session.finish_before(Instant::now() - Duration::from_millis(1));
    let (value, evidence) = read(&mut session, (263, 0x8001, 0x0040), true);
    assert_eq!(value, None);
    assert!(evidence.reason.unwrap().contains("deadline"));
}

#[test]
fn signed_optical_power_when_ani_g_get_contains_negative_measurement() {
    let block = AttributeCodec::decode(
        MeRegistry::get(263).unwrap(),
        0x0040,
        &[0xe7, 0x96],
        AttributeContext::GetResponse,
    );
    let actual = parse(&block, true);
    assert_eq!(actual, Some((-12.5, "e796".into())));
}

#[test]
fn real_zero_when_ani_g_get_returns_zero() {
    let block = AttributeCodec::decode(
        MeRegistry::get(263).unwrap(),
        0x0004,
        &[0, 0],
        AttributeContext::GetResponse,
    );
    let actual = parse(&block, true);
    assert_eq!(actual, Some((0.0, "0000".into())));
}

#[test]
fn fractional_seconds_when_sysuptime_is_in_ten_millisecond_ticks() {
    let block = AttributeCodec::decode(
        MeRegistry::get(257).unwrap(),
        0x0040,
        &[0x00, 0x2b, 0x42, 0xa1],
        AttributeContext::GetResponse,
    );
    let actual = parse(&block, false);
    assert_eq!(actual, Some((28351.05, "002b42a1".into())));
}

#[test]
fn unavailable_when_get_attribute_is_truncated() {
    let block = AttributeCodec::decode(
        MeRegistry::get(263).unwrap(),
        0x0040,
        &[0xe7],
        AttributeContext::GetResponse,
    );
    let actual = parse(&block, true);
    assert_eq!(actual, None);
}

#[test]
fn null_numeric_values_when_onu_is_offline() {
    let sample = snapshot(250, false);
    let actual = serde_json::json!({"rx": sample.rx, "tx": sample.tx, "uptime": sample.uptime,
        "evidence": sample.measurements});
    assert!(actual["rx"].is_null() && actual["tx"].is_null() && actual["uptime"].is_null());
    assert_eq!(
        actual["evidence"]["optical_rx_power_dbm"]["reason"],
        "onu_not_operational"
    );
}

#[test]
fn unavailable_when_cached_registration_cancelled() {
    let token = Arc::new(AtomicBool::new(true));
    cache().lock().unwrap().insert(
        249,
        Cached {
            fec_generation: crate::optical::fec_generation(),
            snapshot: Snapshot {
                fec: fec::unavailable("test_sample"),
                rx: Some(-12.5),
                tx: Some(2.0),
                uptime: Some(42.0),
                inventory: None,
                measurements: Measurements::new(),
            },
            at: Instant::now(),
            token,
        },
    );
    let actual = snapshot(249, true);
    assert_eq!(actual.rx, None);
    assert_eq!(
        actual.measurements["optical_rx_power_dbm"]
            .reason
            .as_deref(),
        Some("stale_or_cancelled_registration")
    );
    cache().lock().unwrap().remove(&249);
}
