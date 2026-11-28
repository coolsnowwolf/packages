use super::Member;
use std::{
    collections::BTreeSet,
    io::{self, Read},
};

pub(super) const MAX_COMPRESSED: usize = 2 * 1024 * 1024;
pub(super) const MAX_DECOMPRESSED: usize = 16 * 1024 * 1024;
pub(super) const MAX_MEMBER: usize = 2 * 1024 * 1024;
pub(super) const MAX_MEMBERS: usize = 128;
const MAX_TEXT: usize = 2 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn read(input: &[u8]) -> io::Result<(Vec<Member>, usize)> {
    if input.len() > MAX_COMPRESSED {
        return Err(invalid("compressed telemetry limit exceeded"));
    }
    let decoder = flate2::read::MultiGzDecoder::new(input);
    let limit = u64::try_from(MAX_DECOMPRESSED + 1).map_err(io::Error::other)?;
    let mut archive = tar::Archive::new(decoder.take(limit));
    let mut names = BTreeSet::new();
    let mut members = Vec::new();
    let mut text_bytes = 0;
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        if members.len() >= MAX_MEMBERS {
            return Err(invalid("telemetry member count exceeded"));
        }
        if !entry.header().entry_type().is_file() {
            return Err(invalid("unsupported tar member type"));
        }
        let raw_path = entry.path_bytes();
        let raw_name =
            std::str::from_utf8(&raw_path).map_err(|_| invalid("invalid tar member path"))?;
        let name = raw_name.strip_prefix("./").unwrap_or(raw_name);
        if name.is_empty()
            || name.contains(['\\', ':'])
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(invalid("unsafe tar member path"));
        }
        if !names.insert(name.to_owned()) {
            return Err(invalid("duplicate tar member"));
        }
        let name = name.to_owned();
        let size = usize::try_from(entry.header().size()?).map_err(io::Error::other)?;
        if size > MAX_MEMBER {
            return Err(invalid("telemetry member size exceeded"));
        }
        let mut bytes = Vec::with_capacity(size);
        entry.read_to_end(&mut bytes)?;
        if bytes.len() != size {
            return Err(invalid("truncated tar member"));
        }
        let (encoding, text, text_omitted) = match String::from_utf8(bytes) {
            Ok(value) if text_bytes + value.len() <= MAX_TEXT => {
                text_bytes += value.len();
                ("utf-8", Some(value), false)
            }
            Ok(_) => ("utf-8", None, true),
            Err(_) => ("binary", None, false),
        };
        members.push(Member {
            name,
            bytes: size,
            encoding: encoding.into(),
            text,
            text_omitted,
        });
    }
    let mut remaining = archive.into_inner();
    let mut chunk = [0; 4096];
    loop {
        let count = remaining.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if chunk[..count].iter().any(|byte| *byte != 0) {
            return Err(invalid("invalid trailing tar data"));
        }
    }
    let expanded_len = usize::try_from(limit - remaining.limit()).map_err(io::Error::other)?;
    if expanded_len > MAX_DECOMPRESSED {
        return Err(invalid("decompressed telemetry limit exceeded"));
    }
    if expanded_len < 1024 || expanded_len % 512 != 0 {
        return Err(invalid("truncated tar container"));
    }
    Ok((members, expanded_len))
}
