//! Checked importer for omci-lib-go v2.2.4, not a general Go parser.
//! All selected fields must match the known constructor grammar or generation fails.
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
    process::Command,
};
const REQUIRED: &[u16] = &[
    2, 7, 11, 45, 47, 84, 130, 131, 134, 135, 136, 137, 148, 157, 171, 256, 257, 262, 263, 264, 266,
    268, 272, 277, 278, 312, 329, 340,
];

fn split(s: &str) -> Vec<String> {
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '{' | '[' if !quoted => depth += 1,
            ')' | '}' | ']' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                result.push(s[start..i].trim().into());
                start = i + 1;
            }
            _ => (),
        }
    }
    assert_eq!(depth, 0, "unbalanced constructor");
    assert!(!quoted);
    result.push(s[start..].trim().into());
    result
}
fn number(s: &str) -> u64 {
    if let Some(s) = s.strip_prefix("0x") {
        u64::from_str_radix(s, 16).unwrap()
    } else {
        s.parse().unwrap()
    }
}
fn b64(s: &str) -> Vec<u8> {
    let mut acc = 0u32;
    let mut bits = 0;
    let mut bytes = Vec::new();
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid base64"),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    bytes
}
fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() == 3 || args.len() == 4,
        "usage: omci-schema GO_REPO OUTPUT [--check]"
    );
    let source = PathBuf::from(&args[1]);
    let version = Command::new("git")
        .args(["describe", "--tags", "--exact-match", "HEAD"])
        .current_dir(&source)
        .output()
        .unwrap();
    assert!(
        version.status.success() && String::from_utf8_lossy(&version.stdout).trim() == "v2.2.4",
        "schema source must be v2.2.4"
    );
    let status = Command::new("git")
        .args(["status", "--porcelain", "--", "generated"])
        .current_dir(&source)
        .output()
        .unwrap();
    assert!(
        status.status.success() && status.stdout.is_empty(),
        "generated Go source must be clean"
    );
    let mut definitions = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut fields = 0;
    for entry in fs::read_dir(source.join("generated")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "go") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        let Some(class_line) = lines
            .iter()
            .find(|s| s.starts_with("const ") && s.contains(" = ClassID("))
        else {
            continue;
        };
        let class = number(
            class_line
                .split(" = ClassID(")
                .nth(1)
                .unwrap()
                .split(')')
                .next()
                .unwrap(),
        ) as u16;
        let Some((name_line, name)) = lines.iter().enumerate().find_map(|(i, s)| {
            s.trim()
                .strip_prefix("Name:")
                .map(|s| (i + 1, s.trim().trim_end_matches(',').trim_matches('"')))
        }) else {
            continue;
        };
        let file = path.file_name().unwrap().to_str().unwrap();
        let reference = format!("Reference {{ id: \"ME-{class:05}\", url: \"https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/{file}#L{name_line}\" }}");
        let required = REQUIRED.contains(&class);
        let mut attrs = String::new();
        let mut mask = 0;
        let mut actions = Vec::new();
        let mut access = "basic";
        if required {
            assert!(
                !text.contains("Constraint:"),
                "unhandled constraint in {file}"
            );
            seen.insert(class);
            mask = number(
                lines
                    .iter()
                    .find_map(|s| s.trim().strip_prefix("AllowedAttributeMask:"))
                    .unwrap()
                    .trim()
                    .trim_end_matches(','),
            );
            access = lines
                .iter()
                .find_map(|s| s.trim().strip_prefix("Access:"))
                .unwrap()
                .trim()
                .trim_end_matches(',');
            let mut in_messages = false;
            let mut in_attributes = false;
            for (line_index, line) in lines.iter().enumerate() {
                let line = line.trim();
                if line.starts_with("MessageTypes:") {
                    in_messages = true;
                    continue;
                }
                if in_messages {
                    if line == ")," {
                        in_messages = false;
                        continue;
                    }
                    actions.push(match line.trim_end_matches(',') {
                        "Create" => 4,
                        "Delete" => 6,
                        "Set" => 8,
                        "Get" => 9,
                        "GetNext" => 26,
                        "MibUpload" => 13,
                        "MibUploadNext" => 14,
                        "MibReset" => 15,
                        "SetTable" => 29,
                        "Test" => 18,
                        "GetCurrentData" => 28,
                        "Reboot" => 25,
                        "SynchronizeTime" => 24,
                        "GetAllAlarms" => 11,
                        "GetAllAlarmsNext" => 12,
                        "ActivateSoftware" => 22,
                        "CommitSoftware" => 23,
                        "StartSoftwareDownload" => 19,
                        "DownloadSection" => 20,
                        "EndSoftwareDownload" => 21,
                        s => panic!("unhandled action {s} in {file}"),
                    });
                    continue;
                }
                if line.starts_with("AttributeDefinitions:") {
                    in_attributes = true;
                    continue;
                }
                if in_attributes && line == "}," {
                    in_attributes = false;
                    continue;
                }
                if !in_attributes {
                    continue;
                }
                let Some((index, call)) = line.split_once(':') else {
                    continue;
                };
                let Ok(index) = index.parse::<u8>() else {
                    continue;
                };
                assert!(index <= 16);
                let call = call.trim().trim_end_matches(',');
                let (ctor, rest) = call.split_once('(').expect("field constructor");
                let a = split(rest.strip_suffix(')').unwrap());
                let multibyte = ctor == "MultiByteField";
                let table = ctor == "TableField";
                let expected = if multibyte { 10 } else { 9 };
                assert_eq!(
                    a.len(),
                    expected,
                    "constructor arguments: {file}:{line_index}"
                );
                let attr_name = if a[0] == "ManagedEntityID" {
                    "ManagedEntityId".into()
                } else {
                    let prefix = format!("const {} = ", a[0]);
                    lines
                        .iter()
                        .find_map(|s| s.strip_prefix(&prefix))
                        .unwrap()
                        .trim_matches('"')
                        .to_string()
                };
                let kind = a[1].strip_suffix("AttributeType").unwrap();
                assert!([
                    "Unknown",
                    "Octets",
                    "String",
                    "UnsignedInteger",
                    "Table",
                    "SignedInteger",
                    "Pointer",
                    "BitField",
                    "Enumeration",
                    "Counter"
                ]
                .contains(&kind));
                let attr_mask = number(&a[2]);
                assert_eq!(attr_mask, if index == 0 { 0 } else { 1 << (16 - index) });
                let size = match ctor {
                    "ByteField" => 1,
                    "Uint16Field" => 2,
                    "Uint32Field" => 4,
                    "Uint64Field" => 8,
                    "MultiByteField" => number(&a[3]) as usize,
                    "TableField" => number(
                        a[3].strip_prefix("TableInfo{nil, ")
                            .expect("unknown table default")
                            .strip_suffix('}')
                            .unwrap(),
                    ) as usize,
                    _ => panic!("unsupported constructor {ctor}"),
                };
                let tail = if multibyte { 5 } else { 4 };
                let permissions = a[tail]
                    .strip_prefix("mapset.NewSetWith(")
                    .unwrap()
                    .strip_suffix(')')
                    .unwrap();
                let permissions: Vec<_> = permissions.split(',').map(str::trim).collect();
                assert!(permissions
                    .iter()
                    .all(|s| ["Read", "Write", "SetByCreate"].contains(s)));
                let default = if table {
                    "None".to_string()
                } else {
                    let value = &a[if multibyte { 4 } else { 3 }];
                    let bytes = if multibyte {
                        b64(value
                            .strip_prefix("toOctets(\"")
                            .unwrap()
                            .strip_suffix("\")")
                            .unwrap())
                    } else {
                        number(value).to_be_bytes()[8 - size..].to_vec()
                    };
                    assert_eq!(bytes.len(), size);
                    format!(
                        "Some(\"{}\")",
                        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    )
                };
                for b in &a[tail + 1..tail + 4] {
                    assert!(b == "true" || b == "false");
                }
                assert_eq!(number(&a[tail + 4]), index as u64);
                let url = format!(
                    "https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/{file}#L{}",
                    line_index + 1
                );
                attrs.push_str(&format!("        AttributeDefinition {{ index: {index}, name: {attr_name:?}, mask: 0x{attr_mask:04x}, kind: AttributeKind::{kind}, size: {size}, read: {}, write: {}, set_by_create: {}, avc: {}, optional: {}, deprecated: {}, default_hex: {default}, reference: Reference {{ id: \"ME-{class:05}\", url: {url:?} }} }},\n",
                    permissions.contains(&"Read"), permissions.contains(&"Write"), permissions.contains(&"SetByCreate"), a[tail+1], a[tail+2], a[tail+3]));
                fields += 1;
            }
            assert!(!attrs.is_empty());
        }
        let actions = actions
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let definition = format!("    // ME-{class:05}: https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/{file}#L{name_line}\n    MeDefinition {{ class_id: {class}, name: {name:?}, allowed_mask: 0x{mask:04x}, access: {access:?}, actions: &[{actions}], layout_supported: {required}, reference: {reference}, attributes: &[\n{attrs}    ] }},\n");
        assert!(
            definitions.insert(class, definition).is_none(),
            "duplicate class {class}"
        );
    }
    assert_eq!(seen.len(), REQUIRED.len(), "missing required class");
    let mut output = "// Copyright (c) 2018-present Boling Consulting Solutions (bcsw.net)\n// Copyright 2020-present Open Networking Foundation\n// SPDX-License-Identifier: Apache-2.0\n// Derived/modified from omci-lib-go v2.2.4 by tools/omci-schema.rs. DO NOT EDIT.\n// Only selected layouts are imported; all remaining classes are name-only.\nuse super::schema::{AttributeDefinition, AttributeKind, MeDefinition, Reference};\npub static DEFINITIONS: &[MeDefinition] = &[\n".to_string();
    for def in definitions.values() {
        output.push_str(def);
    }
    output.push_str("];\n");
    let license = fs::read(source.join("LICENSE")).unwrap();
    let license_path = PathBuf::from(&args[2]).parent().unwrap().join("LICENSE");
    if args.get(3).is_some_and(|s| s == "--check") {
        assert_eq!(
            fs::read_to_string(&args[2]).unwrap(),
            output,
            "generated schema drift"
        );
        assert_eq!(
            fs::read(license_path).unwrap(),
            license,
            "upstream license drift"
        );
    } else {
        assert_eq!(args.len(), 3, "unknown generator option");
        fs::write(&args[2], output).unwrap();
        fs::write(license_path, license).unwrap();
    }
    println!(
        "verified v2.2.4: {} named classes, {} complete layouts, {fields} fields",
        definitions.len(),
        seen.len()
    );
}
