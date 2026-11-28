use std::io::Write;
use std::process::{Command, Stdio};

pub(crate) fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub(crate) fn valid_interface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

pub(crate) fn render_delete_script(bridge_table: Option<&str>, ip_table: &str) -> String {
    let mut out = String::new();
    if let Some(table) = bridge_table {
        out.push_str(&format!(
            "table bridge {table}\ndelete table bridge {table}\n"
        ));
    }
    out.push_str(&format!(
        "table ip {ip_table}\ndelete table ip {ip_table}\n"
    ));
    out
}

pub(crate) fn render_prerouting_script(
    bridge_table: Option<&str>,
    ip_table: &str,
    bridge_rules: &[String],
    ip_rules: &[String],
) -> String {
    if bridge_rules.is_empty() && ip_rules.is_empty() {
        return render_delete_script(bridge_table, ip_table);
    }
    let mut out = String::new();
    if let Some(table) = bridge_table {
        out.push_str(&format!(
            "table bridge {table}\ndelete table bridge {table}\n"
        ));
        if !bridge_rules.is_empty() {
            out.push_str(&format!(
                "table bridge {table} {{\n    chain prerouting {{\n        type filter hook prerouting priority -300; policy accept;\n"
            ));
            for rule in bridge_rules {
                out.push_str(&format!("        {rule}\n"));
            }
            out.push_str("    }\n}\n");
        }
    }
    out.push_str(&format!(
        "table ip {ip_table}\ndelete table ip {ip_table}\n"
    ));
    if !ip_rules.is_empty() {
        out.push_str(&format!(
            "table ip {ip_table} {{\n    chain prerouting {{\n        type nat hook prerouting priority -100; policy accept;\n"
        ));
        for rule in ip_rules {
            out.push_str(&format!("        {rule}\n"));
        }
        out.push_str("    }\n}\n");
    }
    out
}

pub(crate) fn run_nft_batch(script: &str) -> Result<(), String> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start nft: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|e| format!("write nft script: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait nft: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("nft exited with {}", output.status)
        } else {
            format!("nft failed: {stderr}")
        });
    }
    Ok(())
}

pub(crate) fn tables_present(
    bridge_table: Option<&str>,
    ip_table: &str,
) -> Result<(bool, bool), String> {
    let output = Command::new("nft")
        .args(["-j", "list", "tables"])
        .output()
        .map_err(|e| format!("cannot list nft tables: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("nft list tables failed: {stderr}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse nft tables: {e}"))?;
    Ok(parse_tables_present(&parsed, bridge_table, ip_table))
}

pub(crate) fn parse_tables_present(
    value: &serde_json::Value,
    bridge_table: Option<&str>,
    ip_table: &str,
) -> (bool, bool) {
    let Some(entries) = value["nftables"].as_array() else {
        return (false, false);
    };
    let mut bridge = false;
    let mut ip = false;
    for entry in entries {
        let table = &entry["table"];
        if let Some(bt) = bridge_table {
            if table["family"] == "bridge" && table["name"] == bt {
                bridge = true;
            }
        }
        if table["family"] == "ip" && table["name"] == ip_table {
            ip = true;
        }
    }
    (bridge, ip)
}
