use std::io::Write;

#[path = "archive_tests.rs"]
mod archive_tests;

pub(super) fn fixture(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(data.len()).unwrap());
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn identity_claim_requires_matching_unicast_mac_and_serial() {
    let report = super::parse(&fixture(&[
        ("SN", b"H3CT12345678\n"),
        ("sn", b"02:AA:BB:CC:DD:EE\n"),
    ]))
    .unwrap();
    assert_eq!(
        super::identity_claim(&report, "02AABBCCDDEE"),
        Some(("H3CT12345678".into(), "02AABBCCDDEE".into()))
    );
    assert!(super::identity_claim(&report, "02AABBCCDDEF").is_none());
    let invalid = super::parse(&fixture(&[("SN", b"invalid"), ("sn", b"02AABBCCDDEE")])).unwrap();
    assert!(super::identity_claim(&invalid, "02AABBCCDDEE").is_none());
}

#[test]
fn hl202_telemetry_decodes_observed_plain_text_members_without_defaults() {
    // Given: sanitized synthetic fields with the authentic archive's names/types.
    let input = fixture(&[
        ("DeviceInfo_devParaStatic.ModelName", b"HL202-DU-C"),
        ("IP", b"192.0.2.10\n"),
        ("conn_type", b"FIBER"),
        ("Sys_Entry.uptime", b"152.84"),
        ("Info_SysDevice.CpuUsage", b"64"),
        ("unknown.txt", "未知信息".as_bytes()),
    ]);
    // When: parsed without disk extraction.
    let report = super::parse(&input).unwrap();
    // Then: only observed values are represented, with unknown text retained.
    assert_eq!(
        report.fields["model"].value,
        serde_json::json!("HL202-DU-C")
    );
    assert_eq!(report.fields["ip"].value, serde_json::json!("192.0.2.10"));
    assert_eq!(
        report.fields["uptime_seconds"].value,
        serde_json::json!(152.84)
    );
    assert!(!report.fields.contains_key("memory_usage"));
    assert_eq!(report.members[5].text.as_deref(), Some("未知信息"));
}

#[test]
fn hl202_telemetry_rejects_duplicates_corruption_and_unsafe_paths() {
    // Given / When / Then: ambiguous or malformed containers never become reports.
    assert!(super::parse(&fixture(&[("IP", b"a"), ("IP", b"b")])).is_err());
    assert!(super::parse(b"not gzip").is_err());
    let mut raw = vec![0u8; 1024];
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes()[..6].copy_from_slice(b"../bad");
    header.set_size(0);
    header.set_cksum();
    raw[..512].copy_from_slice(header.as_bytes());
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw).unwrap();
    assert!(super::parse(&encoder.finish().unwrap()).is_err());
}

#[test]
fn hl202_telemetry_bad_known_values_are_partial_not_invented() {
    // Given: valid archive but invalid numeric/address fields and a binary unknown member.
    let input = fixture(&[
        ("IP", b"not an IP"),
        ("Sys_Entry.uptime", b"NaN"),
        ("unknown.bin", &[255]),
    ]);
    // When: decoded.
    let report = super::parse(&input).unwrap();
    // Then: errors carry member provenance, and unsupported fields remain absent.
    assert!(report.fields.is_empty());
    assert_eq!(report.errors.len(), 2);
    assert!(report.members[2].text.is_none());
}

#[test]
fn hl202_telemetry_member_count_and_size_limits_are_inclusive() {
    let exact = vec![b'a'; super::archive::MAX_MEMBER];
    assert!(super::parse(&fixture(&[("unknown", &exact)])).is_ok());
    let oversized = vec![b'a'; super::archive::MAX_MEMBER + 1];
    assert!(super::parse(&fixture(&[("unknown", &oversized)])).is_err());
    let names: Vec<String> = (0..129).map(|n| format!("member{n}")).collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), b"".as_slice()))
        .collect();
    assert!(super::parse(&fixture(&entries[..128])).is_ok());
    assert!(super::parse(&fixture(&entries)).is_err());
}

#[test]
fn hl202_telemetry_decompression_limit_counts_headers_and_trailing_data() {
    let base = fixture(&[("member", b"x")]);
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(base.as_slice()), &mut raw)
        .unwrap();
    raw.resize(super::archive::MAX_DECOMPRESSED, 0);
    let encode = |bytes: &[u8]| {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(bytes).unwrap();
        gzip.finish().unwrap()
    };
    assert!(super::parse(&encode(&raw)).is_ok());
    raw.push(0);
    assert!(super::parse(&encode(&raw)).is_err());
    assert!(super::parse(&vec![0; super::archive::MAX_COMPRESSED + 1]).is_err());
}

#[test]
fn hl202_telemetry_text_budget_omits_text_without_losing_member_metadata() {
    let exact = vec![b'a'; super::archive::MAX_MEMBER];
    let report = super::parse(&fixture(&[("first", &exact), ("second", b"x")])).unwrap();
    assert_eq!(
        report.members[0].text.as_ref().unwrap().len(),
        super::archive::MAX_MEMBER
    );
    assert!(report.members[1].text_omitted);
    assert_eq!(report.members[1].bytes, 1);
    assert!(report.members[1].text.is_none());
}

#[test]
fn hl202_telemetry_rejects_links_bad_crc_and_truncated_members() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_link_name("target").unwrap();
    header.set_size(0);
    header.set_cksum();
    builder
        .append_data(&mut header, "link", b"".as_slice())
        .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    assert!(super::parse(&encoder.finish().unwrap()).is_err());
    let mut input = fixture(&[("IP", b"192.0.2.10")]);
    let crc = input.len() - 8;
    input[crc] ^= 1;
    assert!(super::parse(&input).is_err());
    input.truncate(input.len() - 10);
    assert!(super::parse(&input).is_err());
}

#[test]
#[ignore = "requires private authentic archive supplied by operator"]
fn hl202_telemetry_private_capture_matches_observed_contract() {
    let path = std::env::var("FTTR_HL202_PRIVATE_SAMPLE").unwrap();
    let report = super::parse(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(report.members.len(), 18);
    assert_eq!(
        report.fields["model"].value,
        serde_json::json!("HL202-DU-C")
    );
    assert_eq!(
        report.fields["ip"].value,
        serde_json::json!("192.168.1.245")
    );
    assert_eq!(
        report.fields["connection_type"].value,
        serde_json::json!("FIBER")
    );
    assert_eq!(
        report.fields["uptime_seconds"].value,
        serde_json::json!(152.84)
    );
    assert!(report.errors.is_empty());
}

#[test]
fn hl202_telemetry_compressed_limit_accepts_exact_size_and_rejects_next_byte() {
    let base = fixture(&[("unknown", b"bounded")]);
    let empty = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast())
        .finish()
        .unwrap();
    let padding = super::archive::MAX_COMPRESSED - base.len();
    let remainder = padding % empty.len();
    let mut exact = Vec::with_capacity(super::archive::MAX_COMPRESSED);
    for _ in 0..padding / empty.len() {
        exact.extend_from_slice(&empty);
    }
    exact.extend_from_slice(&base[..10]);
    if remainder > 0 {
        let header_flag = exact.len() - 7;
        exact[header_flag] |= 16;
        exact.extend(vec![b'x'; remainder - 1]);
        exact.push(0);
    }
    exact.extend_from_slice(&base[10..]);
    assert_eq!(exact.len(), super::archive::MAX_COMPRESSED);
    assert!(super::parse(&exact).is_ok());
    exact.push(0);
    assert!(super::parse(&exact).is_err());
}

#[test]
fn hl202_telemetry_json_limit_accepts_exact_size_and_rejects_next_byte() {
    let mut report = super::parse(&fixture(&[("unknown", b"")])).unwrap();
    let overhead = serde_json::to_vec(&report).unwrap().len();
    let escaped = "\"\\\n\r\t\0未知";
    let escaped_bytes = serde_json::to_vec(escaped).unwrap().len() - 2;
    report.members[0].text = Some(format!(
        "{escaped}{}",
        "x".repeat(4 * 1024 * 1024 - overhead - escaped_bytes)
    ));
    assert_eq!(serde_json::to_vec(&report).unwrap().len(), 4 * 1024 * 1024);
    assert!(super::report_bytes(&report).is_ok());
    report.members[0].text.as_mut().unwrap().push('x');
    let error = super::report_bytes(&report).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "telemetry JSON limit exceeded");
}
