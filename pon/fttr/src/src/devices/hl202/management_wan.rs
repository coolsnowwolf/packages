//! Pre-Inform management WAN discovery/configuration. Never selects a fixed PVC instance.
use super::{command, mqtt_lifecycle::mqtt_target};
use std::{collections::BTreeSet, time::Duration};

#[derive(Debug)]
struct Connection {
    parent: u32,
    child: u32,
    service: String,
    vlan: String,
    priority: String,
    active: String,
    mode: String,
    isp: String,
    settings_verified: bool,
}
async fn execute(transaction: &command::Transaction, script: &str) -> Result<String, String> {
    let result = transaction
        .execute(script, Duration::from_secs(60))
        .await
        .map_err(|_| "management WAN command transport failed")?;
    if !result.success() {
        return Err(format!(
            "management WAN command failed (status={}, exit={:?}); rediscovery required",
            result.status, result.exit_code
        ));
    }
    result
        .output
        .ok_or("management WAN command returned no result".into())
}
fn parse(text: &str) -> Result<(BTreeSet<u32>, Vec<Connection>), String> {
    let mut parents = BTreeSet::new();
    let mut connections = Vec::new();
    let mut complete = false;
    for line in text.lines() {
        if line == "FTTR_WAN_COMPLETE" {
            complete = true;
            continue;
        }
        let fields: Vec<_> = line.split('|').collect();
        match fields.as_slice() {
            ["P", p] => {
                parents.insert(p.parse().map_err(|_| "invalid WAN parent discovery")?);
            }
            ["C", p, c, service, vlan, priority, active, mode, isp, verified]
                if !service.is_empty() && !service.contains("node information") =>
            {
                connections.push(Connection {
                    parent: p.parse().map_err(|_| "invalid WAN parent")?,
                    child: c.parse().map_err(|_| "invalid WAN child")?,
                    service: service.to_string(),
                    vlan: vlan.to_string(),
                    priority: priority.to_string(),
                    active: active.to_string(),
                    mode: mode.to_string(),
                    isp: isp.to_string(),
                    settings_verified: *verified == "1",
                });
            }
            _ => return Err("invalid management WAN inventory".into()),
        }
    }
    if !complete
        || connections.iter().any(|c| !parents.contains(&c.parent))
        || connections.iter().enumerate().any(|(i, c)| {
            connections[..i]
                .iter()
                .any(|o| o.parent == c.parent && o.child == c.child)
        })
    {
        return Err("management WAN inventory is incomplete or inconsistent".into());
    }
    Ok((parents, connections))
}
const INVENTORY: &str = r#"root=$(tcapi show Wan) || exit 1
printf '%s\n' "$root" | grep -q '\[Wan\]:' || exit 1
parents=$(printf '%s\n' "$root" | awk '/\[PVC\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
for p in $parents; do
    parent=Wan_PVC${p}
    tree=$(tcapi show "$parent") || exit 1
    printf '%s\n' "$tree" | grep -q '\[PVC\]:' || exit 1
    printf 'P|%s\n' "$p"
    children=$(printf '%s\n' "$tree" | awk '/\[Entry\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
    for c in $children; do
        n=${parent}_Entry${c}; valid=1
        for pair in IPVERSION:IPv4 LinkMode:linkIP NATENABLE:Disabled dot1q:Yes dot1p:Yes; do
            key=${pair%%:*}; expected=${pair#*:}
            [ "$(tcapi get "$n" "$key")" = "$expected" ] || valid=0
        done
        for key in LAN1 LAN2 LAN3 LAN4 SSID1 SSID2 SSID3 SSID4 SSIDAC1 SSIDAC2 SSIDAC3 SSIDAC4; do
            [ "$(tcapi get "$n" "$key")" = No ] || valid=0
        done
        [ "$(tcapi get "$parent" VLANMode)" = TAG ] || valid=0
        [ "$(tcapi get "$n" VLANID)" = "$(tcapi get "$parent" VLANID)" ] || valid=0
        [ "$(tcapi get "$n" dot1pData)" = "$(tcapi get "$parent" DOT1P)" ] || valid=0
        printf 'C|%s|%s|%s|%s|%s|%s|%s|%s|%s\n' "$p" "$c" "$(tcapi get "$n" ServiceList)" "$(tcapi get "$parent" VLANID)" "$(tcapi get "$parent" DOT1P)" "$(tcapi get "$n" Active)" "$(tcapi get "$n" WanMode)" "$(tcapi get "$n" ISP)" "$valid"
    done
done
printf 'FTTR_WAN_COMPLETE\n'"#;

fn select_vlan(vlans: &[u32], existing: Option<&Connection>) -> u32 {
    existing
        .filter(|c| c.service.trim() == "TR069")
        .and_then(|c| c.vlan.parse::<u32>().ok())
        .filter(|v| vlans.contains(v))
        .unwrap_or(vlans[0])
}
pub fn ensure(
    target: &str,
    vlans: &[u32],
    priority: u32,
    persist_matching: bool,
    mut progress: impl FnMut(super::super::ManagementWanStage) -> bool,
) -> Result<u32, String> {
    crate::runtime::block_on(ensure_async(
        target,
        vlans,
        priority,
        persist_matching,
        |stage| std::future::ready(progress(stage)),
    ))
}
pub async fn ensure_async<F: std::future::Future<Output = bool>>(
    target: &str,
    vlans: &[u32],
    priority: u32,
    persist_matching: bool,
    mut progress: impl FnMut(super::super::ManagementWanStage) -> F,
) -> Result<u32, String> {
    if vlans.is_empty() || vlans.iter().any(|v| *v == 0 || *v > 4094) || priority > 7 {
        return Err("invalid management VLAN/priority".into());
    }
    let target = mqtt_target(target).ok_or("invalid MQTT management target")?;
    let transaction =
        command::Transaction::acquire(&target).map_err(|_| "device command transaction active")?;
    let (parents, connections) = parse(execute(&transaction, INVENTORY).await?.trim())?;
    let tr069: Vec<_> = connections
        .iter()
        .filter(|c| c.service.to_uppercase().contains("TR069"))
        .collect();
    if tr069.len() > 1 {
        return Err("multiple TR069 connections require explicit resolution".into());
    }
    let vlan = select_vlan(vlans, tr069.first().copied());
    let (p, c, create) = if let Some(connection) = tr069.first() {
        if connection.service.trim() != "TR069" {
            return Err(
                "combined-service TR069 WAN cannot be rewritten as a dedicated management WAN"
                    .into(),
            );
        }
        if connections
            .iter()
            .any(|other| other.parent == connection.parent && other.child != connection.child)
        {
            return Err("TR069 WAN shares VLAN settings with another connection".into());
        }
        if matches(connection, vlan, priority) && !persist_matching {
            return Ok(vlan);
        }
        (connection.parent, connection.child, false)
    } else {
        let p = (0..=parents.len() as u32)
            .find(|p| !parents.contains(p))
            .ok_or("no free management WAN parent")?;
        (p, 0, true)
    };
    let parent = format!("Wan_PVC{p}");
    let node = format!("{parent}_Entry{c}");
    if !progress(super::super::ManagementWanStage::Configuring).await {
        return Err("management WAN authorization changed before configuration".into());
    }
    let mut commands = Vec::new();
    if create {
        // First mutation leaves a discoverable service marker; never blindly replay creation after a timeout.
        commands.push(format!("tcapi set {node} ServiceList TR069 || exit 1"));
    }
    if connections
        .iter()
        .any(|x| x.parent == p && x.child == c && matches(x, vlan, priority))
    {
        commands.push("tcapi commit Wan && tcapi save || exit 1".into());
    } else {
        commands.push(format!("tcapi set {node} Active No || exit 1"));
        for (n, key, value) in [
            (&parent, "VLANMode", "TAG".into()),
            (&parent, "VLANID", vlan.to_string()),
            (&parent, "DOT1P", priority.to_string()),
            (&parent, "EPONEnable", "Yes".into()),
            (&parent, "GPONEnable", "Yes".into()),
            (&node, "WanMode", "Route".into()),
            (&node, "LinkMode", "linkIP".into()),
            (&node, "IPVERSION", "IPv4".into()),
            (&node, "ISP", "0".into()),
            (&node, "VLANMode", "TAG".into()),
            (&node, "VLANID", vlan.to_string()),
            (&node, "dot1q", "Yes".into()),
            (&node, "dot1p", "Yes".into()),
            (&node, "dot1pData", priority.to_string()),
            (&node, "NATENABLE", "Disabled".into()),
        ] {
            commands.push(format!("if [ \"$(tcapi get {n} {key})\" != '{value}' ]; then tcapi set {n} {key} '{value}' || exit 1; fi"));
        }
        for key in [
            "LAN1", "LAN2", "LAN3", "LAN4", "SSID1", "SSID2", "SSID3", "SSID4", "SSIDAC1",
            "SSIDAC2", "SSIDAC3", "SSIDAC4",
        ] {
            commands.push(format!("tcapi set {node} {key} No || exit 1"));
        }
        commands.push(format!(
            "tcapi set {node} Active Yes && tcapi commit Wan && tcapi save || exit 1"
        ));
    }
    commands.push("printf 'FTTR_MANAGEMENT_WAN_WRITTEN\\n'".into());
    if !execute(&transaction, &commands.join("; "))
        .await?
        .lines()
        .any(|l| l == "FTTR_MANAGEMENT_WAN_WRITTEN")
    {
        return Err("management WAN write result unknown; rediscovery required".into());
    }
    if !progress(super::super::ManagementWanStage::Verifying).await {
        return Err("management WAN authorization changed before verification".into());
    }
    let (_, connections) = parse(execute(&transaction, INVENTORY).await?.trim())?;
    if !connections
        .iter()
        .any(|x| x.parent == p && x.child == c && matches(x, vlan, priority))
    {
        return Err("management WAN readback does not match".into());
    }
    Ok(vlan)
}
fn matches(c: &Connection, vlan: u32, priority: u32) -> bool {
    c.service == "TR069"
        && c.vlan == vlan.to_string()
        && c.priority == priority.to_string()
        && c.active == "Yes"
        && c.mode == "Route"
        && c.isp == "0"
        && c.settings_verified
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_uses_actual_instances_and_requires_completion() {
        let (p, c) = parse("P|7\nC|7|2|TR069|46|0|Yes|Route|0|1\nFTTR_WAN_COMPLETE").unwrap();
        assert!(p.contains(&7));
        assert_eq!(c[0].child, 2);
        assert!(matches(&c[0], 46, 0));
        assert!(parse("P|7").is_err());
        assert!(parse("unexpected\nFTTR_WAN_COMPLETE").is_err());
        assert!(parse("C|7|2|TR069|46|0|Yes|Route|0|1\nFTTR_WAN_COMPLETE").is_err());
    }
    #[test]
    fn existing_dedicated_vlan_wins_over_fallback_even_when_settings_need_repair() {
        let (_, mut c) = parse("P|7\nC|7|2|TR069|46|7|No|Route|0|0\nFTTR_WAN_COMPLETE").unwrap();
        assert_eq!(select_vlan(&[47, 46], Some(&c[0])), 46);
        assert_eq!(select_vlan(&[47, 48], Some(&c[0])), 47);
        assert_eq!(select_vlan(&[47, 46], None), 47);
        c[0].service = "TR069_INTERNET".into();
        assert_eq!(select_vlan(&[47, 46], Some(&c[0])), 47);
    }
}
