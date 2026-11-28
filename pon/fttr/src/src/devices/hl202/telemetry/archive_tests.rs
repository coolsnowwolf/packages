use std::io::{Read, Write};

fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}

fn raw_fixture() -> Vec<u8> {
    let input = super::fixture(&[("IP", b"192.0.2.10")]);
    let mut raw = Vec::new();
    flate2::read::MultiGzDecoder::new(input.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    raw
}

#[test]
fn report_is_identical_when_tar_crosses_gzip_member_boundaries() {
    let raw = raw_fixture();
    let expected = super::super::parse(&gzip(&raw)).unwrap();
    let input: Vec<u8> = raw.chunks(137).flat_map(gzip).collect();

    let actual = super::super::parse(&input).unwrap();

    let mut expected = serde_json::to_value(expected).unwrap();
    expected["compressed_bytes"] = serde_json::json!(input.len());
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
}

#[test]
fn invalid_tail_is_rejected_when_after_tar_terminator() {
    let mut raw = raw_fixture();
    raw.extend_from_slice(&[1; 512]);

    let result = super::super::parse(&gzip(&raw));

    assert!(result.is_err());
}

#[test]
fn partial_block_is_rejected_when_tail_is_zero() {
    let mut raw = raw_fixture();
    raw.push(0);

    let result = super::super::parse(&gzip(&raw));

    assert!(result.is_err());
}

#[test]
fn short_container_is_rejected_when_single_zero_block() {
    let input = gzip(&[0; 512]);

    let result = super::super::parse(&input);

    assert!(result.is_err());
}

#[test]
fn bad_crc_is_rejected_when_in_trailing_gzip_member() {
    let mut input = gzip(&raw_fixture());
    let mut trailing = gzip(&[0; 512]);
    let crc = trailing.len() - 8;
    trailing[crc] ^= 1;
    input.extend(trailing);

    let result = super::super::parse(&input);

    assert!(result.is_err());
}

#[test]
fn invalid_gzip_is_rejected_when_after_valid_archive() {
    let mut input = gzip(&raw_fixture());
    input.extend_from_slice(b"not another gzip member");

    let result = super::super::parse(&input);

    assert!(result.is_err());
}

#[test]
fn expansion_limit_is_enforced_when_padding_is_in_another_gzip_member() {
    let raw = raw_fixture();
    let mut input = gzip(&raw);
    input.extend(gzip(&vec![
        0;
        super::super::archive::MAX_DECOMPRESSED + 512
            - raw.len()
    ]));

    let result = super::super::parse(&input);

    assert!(result.is_err());
}

#[test]
fn bad_header_checksum_is_rejected_when_member_is_otherwise_valid() {
    let mut raw = raw_fixture();
    raw[0] ^= 1;

    let result = super::super::parse(&gzip(&raw));

    assert!(result.is_err());
}
