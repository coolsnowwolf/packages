//! HL202 native tcapi contract. No CWMP paths or instance numbers cross this boundary.
use super::{command::CommandStatus, command_binding, command_transport::PublicationState};
use crate::{
    mqtt_identity::Selection,
    provisioning::{model::*, runtime_store, storage},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Fields = BTreeMap<(String, String), String>;
#[derive(Clone, Default)]
struct Inventory {
    fields: Fields,
    parents: BTreeSet<u32>,
    children: BTreeSet<(u32, u32)>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Ownership {
    #[serde(default = "legacy_schema")]
    schema_version: u32,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    operation_id: u64,
    #[serde(default)]
    binding_generation: String,
    #[serde(default)]
    wan: BTreeMap<String, Owned>,
    #[serde(default)]
    pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_tx: Option<PendingTransaction>,
}
fn legacy_schema() -> u32 {
    1
}
#[derive(Clone, Serialize, Deserialize)]
struct PendingTransaction {
    request_id: String,
    boot_id: String,
    dispatched_at: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    expected_fields: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    expected_secret_macs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_wans: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    item_fields: BTreeMap<String, Vec<(String, String)>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    item_meta: BTreeMap<String, (ItemKind, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_state: Option<String>,
    #[serde(default)]
    resume_attempts: u32,
}
#[derive(Clone, Serialize, Deserialize)]
struct Owned {
    parent: u32,
    child: u32,
    service: String,
    vlan: String,
}
struct Desired {
    fields: Fields,
    items: BTreeMap<String, BTreeSet<(String, String)>>,
    owned: Ownership,
    removed: BTreeSet<(u32, u32)>,
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn execute(
    transaction: &super::command::Transaction,
    selection: &Selection,
    script: &str,
) -> Result<String, String> {
    execute_once(transaction, selection, script)
        .await
        .map_err(|(reason, _)| reason)
}

pub(super) async fn execute_once(
    transaction: &super::command::Transaction,
    selection: &Selection,
    script: &str,
) -> Result<String, (String, bool)> {
    let reply = command_binding::execute_transaction_async(
        transaction,
        selection,
        script,
        Duration::from_secs(15),
    )
    .await
    .map_err(|_| {
        (
            "command transport failed; rediscovery required".into(),
            false,
        )
    })?;
    inventory_reply(reply)
}

fn inventory_reply(reply: command_binding::SelectedResult) -> Result<String, (String, bool)> {
    if !reply.binding_current {
        return Err((
            "command identity changed; rediscovery required".into(),
            false,
        ));
    }
    if reply.result.status == CommandStatus::Timeout {
        return Err((
            "command response timed out; execution outcome unknown".into(),
            true,
        ));
    }
    if reply.result.status != CommandStatus::Executed || reply.result.exit_code != Some(0) {
        return Err((format!("command did not complete successfully (status={:?}, exit={:?}); rediscovery required", reply.result.status, reply.result.exit_code), false));
    }
    if reply.result.output_truncated {
        return Err((
            "command output truncated; rediscovery required".into(),
            false,
        ));
    }
    reply
        .result
        .output
        .ok_or(("command returned no framed output".into(), false))
}

async fn initial_inventory_with_retry<F, Fut, Check>(
    mut read: F,
    check_auth: Check,
    retry_delay: Duration,
) -> Result<String, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String, (String, bool)>>,
    Check: Fn() -> Result<(), String>,
{
    match read().await {
        Err((_, true)) => {
            tokio::time::sleep(retry_delay).await;
            check_auth()?;
            read().await.map_err(|(reason, _)| reason)
        }
        Err((reason, _)) => Err(reason),
        Ok(text) => Ok(text),
    }
}

pub(super) const BOOT_PROBE: &str = r#"boot=; read -r boot </proc/sys/kernel/random/boot_id 2>/dev/null || boot=
restart=0; command -v reboot >/dev/null 2>&1 && restart=1
printf 'BOOT|%s|%s\n' "$boot" "$restart""#;

pub(super) fn valid_boot_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && id.bytes().any(|b| !matches!(b, b'0' | b'-'))
}

pub(super) fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(super) fn wrapped_write_script(
    inner_script: &str,
    request_id: &str,
    expected_boot_id: &str,
    operation_id: u64,
) -> Result<String, String> {
    if !valid_request_id(request_id) {
        return Err("invalid execution request ID".into());
    }
    if !valid_boot_id(expected_boot_id) {
        return Err("invalid expected ONU boot ID".into());
    }
    let inner = inner_script.trim_end();
    Ok(format!(
        r#"fttr_exec_dir=/tmp/.fttr-exec
fttr_rec_dir="$fttr_exec_dir/receipts"
fttr_lock_dir="$fttr_exec_dir/writer.lock.d"
fttr_active="$fttr_exec_dir/active"
fttr_req='{request_id}'
fttr_exp_boot='{expected_boot_id}'
fttr_op='{operation_id}'
mkdir -p "$fttr_rec_dir" || exit 70
if ! mkdir "$fttr_lock_dir" 2>/dev/null; then
 printf 'FTTR_EXEC_BUSY\n'
 exit 75
fi
fttr_boot=; read -r fttr_boot </proc/sys/kernel/random/boot_id 2>/dev/null || fttr_boot=
if [ "$fttr_boot" != "$fttr_exp_boot" ]; then
 rmdir "$fttr_lock_dir" 2>/dev/null || true
 printf 'FTTR_EXEC_BOOT_MISMATCH|%s\n' "$fttr_boot"
 exit 76
fi
fttr_rec="$fttr_rec_dir/$fttr_req"
if [ -f "$fttr_rec" ]; then
 IFS= read -r fttr_prev <"$fttr_rec" || fttr_prev=
 case "$fttr_prev" in
  'terminal|rc='*'|boot='*)
   fttr_rc=${{fttr_prev#terminal|rc=}}
   fttr_rc=${{fttr_rc%%|*}}
   fttr_prev_boot=${{fttr_prev#*|boot=}}
   fttr_prev_boot=${{fttr_prev_boot%%|*}}
   if [ "$fttr_prev_boot" = "$fttr_boot" ]; then
    rmdir "$fttr_lock_dir" 2>/dev/null || true
    printf 'FTTR_EXEC_DEDUP|%s\n' "$fttr_rc"
    [ "$fttr_rc" = "0" ] && printf 'FTTR_WRITE_COMPLETE\n'
    exit "$fttr_rc"
   fi
   ;;
 esac
fi
if ! (printf 'running|boot=%s|op=%s|pid=%s\n' "$fttr_boot" "$fttr_op" "$$" >"${{fttr_rec}}.tmp" && mv -f "${{fttr_rec}}.tmp" "$fttr_rec"); then
 rmdir "$fttr_lock_dir" 2>/dev/null || true
 exit 72
fi
printf '%s|%s|%s|%s\n' "$fttr_req" "$fttr_op" "$fttr_boot" "$$" >"${{fttr_active}}.tmp" && mv -f "${{fttr_active}}.tmp" "$fttr_active" || true
(
{inner}
)
fttr_rc=$?
printf 'terminal|rc=%s|boot=%s|op=%s\n' "$fttr_rc" "$fttr_boot" "$fttr_op" >"${{fttr_rec}}.tmp" && mv -f "${{fttr_rec}}.tmp" "$fttr_rec"
rm -f "$fttr_active"
rmdir "$fttr_lock_dir" 2>/dev/null || true
fttr_keep=0
for fttr_old in $(ls -1t "$fttr_rec_dir" 2>/dev/null); do
 case "$fttr_old" in
  *.tmp|"$fttr_req") continue ;;
 esac
 fttr_keep=$((fttr_keep + 1))
 if [ "$fttr_keep" -gt 16 ]; then
  rm -f "$fttr_rec_dir/$fttr_old"
 fi
done
if [ "$fttr_rc" -eq 0 ]; then
 printf 'FTTR_WRITE_COMPLETE\n'
else
 printf 'FTTR_WRITE_FAILED|%s\n' "$fttr_rc"
fi
exit "$fttr_rc""#
    ))
}

pub(super) fn remote_status_probe(
    request_id: &str,
    expected_boot_id: &str,
) -> Result<String, String> {
    if !valid_request_id(request_id) {
        return Err("invalid execution request ID".into());
    }
    if !valid_boot_id(expected_boot_id) {
        return Err("invalid expected ONU boot ID".into());
    }
    Ok(format!(
        r#"fttr_exec_dir=/tmp/.fttr-exec
fttr_rec="$fttr_exec_dir/receipts/{request_id}"
fttr_lock_dir="$fttr_exec_dir/writer.lock.d"
fttr_active="$fttr_exec_dir/active"
fttr_exp_boot='{expected_boot_id}'
fttr_boot=; read -r fttr_boot </proc/sys/kernel/random/boot_id 2>/dev/null || fttr_boot=
if [ "$fttr_boot" != "$fttr_exp_boot" ]; then
 printf 'EXEC_STATUS|rebooted||%s\n' "$fttr_boot"
elif [ -f "$fttr_rec" ]; then
 IFS= read -r fttr_line <"$fttr_rec" || fttr_line=
 case "$fttr_line" in
  'terminal|rc='*'|boot='*)
   fttr_rc=${{fttr_line#terminal|rc=}}
   fttr_rc=${{fttr_rc%%|*}}
   fttr_rboot=${{fttr_line#*|boot=}}
   fttr_rboot=${{fttr_rboot%%|*}}
   if [ "$fttr_rboot" = "$fttr_boot" ]; then
    printf 'EXEC_STATUS|finished|%s|%s\n' "$fttr_rc" "$fttr_boot"
   else
    printf 'EXEC_STATUS|unknown||%s\n' "$fttr_boot"
   fi
   ;;
  'running|boot='*'|pid='*)
   fttr_rboot=${{fttr_line#*|boot=}}
   fttr_rboot=${{fttr_rboot%%|*}}
   fttr_rpid=${{fttr_line##*|pid=}}
   if [ "$fttr_rboot" = "$fttr_boot" ] && [ -d "$fttr_lock_dir" ] && [ -n "$fttr_rpid" ] && [ -d "/proc/$fttr_rpid" ] && tr '\0' ' ' <"/proc/$fttr_rpid/cmdline" 2>/dev/null | grep -q 'sh'; then
    printf 'EXEC_STATUS|running|%s|%s\n' "$fttr_rpid" "$fttr_boot"
   else
    printf 'EXEC_STATUS|aborted||%s\n' "$fttr_boot"
   fi
   ;;
  *)
   printf 'EXEC_STATUS|unknown||%s\n' "$fttr_boot"
   ;;
 esac
elif [ -d "$fttr_lock_dir" ]; then
 fttr_act=
 [ -f "$fttr_active" ] && IFS= read -r fttr_act <"$fttr_active" || fttr_act=
 printf 'EXEC_STATUS|running|%s|%s\n' "$fttr_act" "$fttr_boot"
else
 printf 'EXEC_STATUS|missing||%s\n' "$fttr_boot"
fi"#
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RemoteExecutionState {
    Running,
    Finished(i32),
    Rebooted(String),
    Aborted,
    Missing,
    Unknown,
}

impl RemoteExecutionState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Finished(_) => "finished",
            Self::Rebooted(_) => "rebooted",
            Self::Aborted => "aborted",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }

    fn confirmed_ended(&self) -> bool {
        matches!(self, Self::Finished(_) | Self::Rebooted(_))
    }

    fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Finished(rc) => Some(*rc),
            _ => None,
        }
    }
}

fn parse_exec_status(line: &str) -> Result<RemoteExecutionState, String> {
    let parts: Vec<_> = line.split('|').collect();
    match parts.as_slice() {
        ["EXEC_STATUS", "running", _, _] => Ok(RemoteExecutionState::Running),
        ["EXEC_STATUS", "rebooted", _, boot] => {
            if valid_boot_id(boot) {
                Ok(RemoteExecutionState::Rebooted((*boot).to_ascii_lowercase()))
            } else {
                Ok(RemoteExecutionState::Unknown)
            }
        }
        ["EXEC_STATUS", "finished", rc, _] => match rc.parse::<i32>() {
            Ok(code) => Ok(RemoteExecutionState::Finished(code)),
            Err(_) => Ok(RemoteExecutionState::Unknown),
        },
        ["EXEC_STATUS", "aborted", _, _] => Ok(RemoteExecutionState::Aborted),
        ["EXEC_STATUS", "missing", _, _] => Ok(RemoteExecutionState::Missing),
        ["EXEC_STATUS", "unknown", _, _] => Ok(RemoteExecutionState::Unknown),
        _ => Err("invalid remote execution status frame".into()),
    }
}

pub(super) const INVENTORY: &str = r#"read_fields() {
 n=$1; shift
 for k in "$@"; do
  v=$(tcapi get "$n" "$k") || exit 1
  printf 'V|%s|%s|' "$n" "$k"
  printf '%s' "$v" | od -An -v -tx1 | tr -d ' \n'
  printf '\n'
 done
}
root=$(tcapi show Wan) || exit 1
printf '%s\n' "$root" | grep -q '\[Wan\]:' || exit 1
parents=$(printf '%s\n' "$root" | awk '/\[PVC\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
for p in $parents; do
 parent=Wan_PVC${p}
 printf 'P|%s\n' "$p"
 read_fields "$parent" VLANMode VLANID DOT1P EPONEnable GPONEnable
 tree=$(tcapi show "$parent") || exit 1
 children=$(printf '%s\n' "$tree" | awk '/\[Entry\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
 for c in $children; do
  printf 'C|%s|%s\n' "$p" "$c"
   read_fields "${parent}_Entry${c}" ServiceList Active WanMode ISP VLANMode VLANID dot1q dot1p dot1pData IPVERSION LinkMode BridgeMode NATENABLE LAN1 LAN2 LAN3 LAN4 SSID1 SSID2 SSID3 SSID4 SSIDAC1 SSIDAC2 SSIDAC3 SSIDAC4
   interface=$(tcapi get "${parent}_Entry${c}" IFName)
   runtime=0
   case "$interface" in nas[0-7]_[0-7]) [ -d "/sys/class/net/$interface" ] && runtime=1 ;; esac
   printf 'V|%s|__runtime_present|3%s\n' "${parent}_Entry${c}" "$runtime"
 done
done
for radio in WLan WLan11ac; do
 read_fields "${radio}_Common" APOn BssidNum
 for slot in 0 1 2 3; do
   read_fields "${radio}_Entry${slot}" SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256
 done
done
printf 'FTTR_INVENTORY_COMPLETE\n'"#;

fn parse(text: &str) -> Result<Inventory, String> {
    let mut inventory = Inventory::default();
    let mut complete = false;
    for line in text.lines() {
        if complete {
            return Err("data after inventory completion".into());
        }
        let parts: Vec<_> = line.split('|').collect();
        match parts.as_slice() {
            ["P", p] => {
                let p = p.parse::<u32>().map_err(|_| "invalid parent")?;
                if p > 7 || !inventory.parents.insert(p) {
                    return Err("duplicate/out of range parent".into());
                }
            }
            ["C", p, c] => {
                let pair = (
                    p.parse::<u32>().map_err(|_| "invalid parent")?,
                    c.parse::<u32>().map_err(|_| "invalid child")?,
                );
                if pair.0 > 7 || pair.1 > 7 || !inventory.children.insert(pair) {
                    return Err("duplicate/out of range child".into());
                }
            }
            ["V", node, key, hex] => {
                if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalid field encoding".into());
                }
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "invalid field encoding")?;
                let value = String::from_utf8(bytes).map_err(|_| "invalid field text")?;
                if inventory
                    .fields
                    .insert((node.to_string(), key.to_string()), value)
                    .is_some()
                {
                    return Err("duplicate field".into());
                }
            }
            ["FTTR_INVENTORY_COMPLETE"] if !complete => complete = true,
            _ => return Err("unexpected or incomplete inventory".into()),
        }
    }
    if !complete
        || inventory
            .children
            .iter()
            .any(|(p, _)| !inventory.parents.contains(p))
    {
        return Err("incomplete inventory".into());
    }
    for radio in ["WLan", "WLan11ac"] {
        if get(&inventory, &format!("{radio}_Common"), "BssidNum") != "4" {
            return Err("unverified Wi-Fi slot layout".into());
        }
    }
    // A terminator alone does not prove every declared object was read. Missing
    // fields must never be interpreted as an absent WAN or an empty credential.
    let mut expected = BTreeSet::new();
    let mut require = |node: String, keys: &str| {
        expected.extend(
            keys.split_whitespace()
                .map(|key| (node.clone(), key.into())),
        );
    };
    for p in &inventory.parents {
        require(
            format!("Wan_PVC{p}"),
            "VLANMode VLANID DOT1P EPONEnable GPONEnable",
        );
    }
    for (p, c) in &inventory.children {
        require(format!("Wan_PVC{p}_Entry{c}"), "ServiceList Active WanMode ISP VLANMode VLANID dot1q dot1p dot1pData IPVERSION LinkMode BridgeMode NATENABLE LAN1 LAN2 LAN3 LAN4 SSID1 SSID2 SSID3 SSID4 SSIDAC1 SSIDAC2 SSIDAC3 SSIDAC4 __runtime_present");
    }
    for radio in ["WLan", "WLan11ac"] {
        require(format!("{radio}_Common"), "APOn BssidNum");
        for slot in 0..4 {
            require(
                format!("{radio}_Entry{slot}"),
                "SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256",
            );
        }
    }
    if expected.len() != inventory.fields.len()
        || expected
            .iter()
            .any(|key| !inventory.fields.contains_key(key))
    {
        return Err("incomplete or out of scope inventory fields".into());
    }
    if inventory.children.iter().any(|(p, c)| {
        !matches!(
            get(
                &inventory,
                &format!("Wan_PVC{p}_Entry{c}"),
                "__runtime_present"
            ),
            "0" | "1"
        )
    }) {
        return Err("invalid runtime presence".into());
    }
    Ok(inventory)
}
fn get<'a>(inv: &'a Inventory, node: &str, key: &str) -> &'a str {
    inv.fields
        .get(&(node.into(), key.into()))
        .map(String::as_str)
        .unwrap_or("")
}
fn protected(service: &str) -> bool {
    service.split(['_', ',', ' ']).any(|s| s == "TR069")
}
fn wlan(endpoint: &EndpointRef) -> Result<String, String> {
    match endpoint {
        EndpointRef::Ssid { radio, slot } if matches!(radio, 2 | 5) && (1..=4).contains(slot) => {
            Ok(format!(
                "{}_Entry{}",
                if *radio == 2 { "WLan" } else { "WLan11ac" },
                slot - 1
            ))
        }
        _ => Err("unsupported native Wi-Fi endpoint".into()),
    }
}
fn binding(endpoint: &EndpointRef) -> Result<String, String> {
    match endpoint {
        EndpointRef::Lan(n) if (1..=4).contains(n) => Ok(format!("LAN{n}")),
        EndpointRef::Ssid { radio, slot } if matches!(radio, 2 | 5) && (1..=4).contains(slot) => {
            Ok(format!("SSID{}{slot}", if *radio == 2 { "" } else { "AC" }))
        }
        _ => Err("unsupported native binding endpoint".into()),
    }
}
impl Desired {
    fn set(&mut self, id: &str, node: &str, key: &str, value: impl ToString) {
        let field = (node.to_string(), key.to_string());
        self.items
            .entry(id.into())
            .or_default()
            .insert(field.clone());
        self.fields.insert(field, value.to_string());
    }
}

fn compile(
    device: &Device,
    inv: &Inventory,
    owned: Ownership,
    full: bool,
) -> Result<Desired, String> {
    let mut desired = Desired {
        fields: Fields::new(),
        items: BTreeMap::new(),
        owned,
        removed: BTreeSet::new(),
    };
    let mut wifi_nodes = BTreeSet::new();
    let mut desired_wan = BTreeSet::new();
    let mut selected_parents = BTreeSet::new();
    for item in &device.items {
        if let Some(reason) = &item.blocked {
            return Err(reason.clone());
        }
        desired.items.entry(item.id.clone()).or_default();
        if let Some(wifi) = &item.wlan_intent {
            let node = wlan(&wifi.endpoint)?;
            if !wifi_nodes.insert(node.clone()) {
                return Err("duplicate desired Wi-Fi endpoint".into());
            }
            desired.set(&item.id, &node, "SSID", &wifi.ssid);
            if let Some(enabled) = wifi.enabled {
                desired.set(&item.id, &node, "EnableSSID", u8::from(enabled));
            }
            if wifi.enabled != Some(false) {
                desired.set(
                    &item.id,
                    &format!("{}_Common", node.split('_').next().unwrap()),
                    "APOn",
                    "1",
                );
            }
            if let Some(broadcast) = wifi.broadcast {
                desired.set(&item.id, &node, "HideSSID", u8::from(!broadcast));
            }
            if let Some(security) = wifi.security {
                let (auth, encrypt, capable, required) = match security {
                    WifiSecurity::None => ("OPEN", "NONE", "0", "0"),
                    WifiSecurity::Wpa2Psk => ("WPA2PSK", "AES", "0", "0"),
                    WifiSecurity::WpaPskWpa2Psk => ("WPAPSKWPA2PSK", "TKIPAES", "0", "0"),
                    WifiSecurity::Wpa3Sae => ("WPA3PSK", "AES", "1", "1"),
                    WifiSecurity::Wpa2PskWpa3Sae => ("WPA2PSKWPA3PSK", "AES", "1", "0"),
                };
                desired.set(&item.id, &node, "AuthMode", auth);
                desired.set(&item.id, &node, "EncrypType", encrypt);
                desired.set(&item.id, &node, "PMFMFPC", capable);
                desired.set(&item.id, &node, "PMFMFPR", required);
                desired.set(&item.id, &node, "PMFSHA256", "0");
            }
            if let Some(key) = &wifi.key {
                desired.set(&item.id, &node, "WPAPSK", key);
            }
        } else if let Some(wan) = &item.wan_intent {
            if wan.services.is_empty()
                || wan
                    .services
                    .iter()
                    .any(|s| protected(s) || !s.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                return Err("invalid or protected WAN service".into());
            }
            if wan.mode != ConnectionMode::Bridge {
                return Err("native routed business WAN contract not verified".into());
            }
            desired_wan.insert(item.id.clone());
            let service = wan.services.join("_");
            let (vlan, mode) = match wan.vlan {
                VlanMode::Untagged => ("4097".to_string(), "TRANSPARENT"),
                VlanMode::Tagged(v) => (v.to_string(), "TAG"),
            };
            let previous = desired.owned.wan.get(&item.id).cloned();
            let candidates: Vec<_> = inv
                .children
                .iter()
                .copied()
                .filter(|(p, c)| {
                    let node = format!("Wan_PVC{p}_Entry{c}");
                    get(inv, &node, "ServiceList") == service
                        && get(inv, &format!("Wan_PVC{p}"), "VLANID") == vlan
                        && get(inv, &node, "WanMode") == "Bridge"
                })
                .collect();
            let pair = if let Some(previous) =
                previous.filter(|p| inv.children.contains(&(p.parent, p.child)))
            {
                let node = format!("Wan_PVC{}_Entry{}", previous.parent, previous.child);
                if get(inv, &node, "ServiceList") != previous.service
                    || get(inv, &format!("Wan_PVC{}", previous.parent), "VLANID") != previous.vlan
                {
                    return Err("owned WAN identity changed; refusing overwrite".into());
                }
                (previous.parent, previous.child)
            } else {
                match candidates.as_slice() {
                    [pair] => *pair,
                    [] => (
                        (0..8)
                            .find(|p| !inv.parents.contains(p) && !selected_parents.contains(p))
                            .ok_or("no free dedicated WAN parent")?,
                        0,
                    ),
                    _ => return Err("multiple matching business WANs; ambiguous ownership".into()),
                }
            };
            let (p, c) = pair;
            if !selected_parents.insert(p)
                || inv
                    .children
                    .iter()
                    .any(|(other, child)| *other == p && *child != c)
            {
                return Err("shared WAN parent is protected from business writes".into());
            }
            let parent = format!("Wan_PVC{p}");
            let node = format!("{parent}_Entry{c}");
            if protected(get(inv, &node, "ServiceList")) {
                return Err("management WAN is protected".into());
            }
            desired.set(&item.id, &parent, "VLANMode", mode);
            desired.set(&item.id, &parent, "VLANID", &vlan);
            desired.set(&item.id, &parent, "DOT1P", wan.priority);
            desired.set(&item.id, &parent, "EPONEnable", "Yes");
            desired.set(&item.id, &parent, "GPONEnable", "Yes");
            for (key, value) in [
                ("ServiceList", service.as_str()),
                ("WanMode", "Bridge"),
                ("LinkMode", "linkIP"),
                ("ISP", "3"),
                ("BridgeMode", "IP_Bridged"),
                ("NATENABLE", "Disabled"),
                ("IPVERSION", "IPv4/IPv6"),
                ("VLANMode", if mode == "TAG" { "TAG" } else { "UNTAG" }),
                ("dot1q", if mode == "TAG" { "Yes" } else { "No" }),
                ("dot1p", if mode == "TAG" { "Yes" } else { "No" }),
            ] {
                desired.set(&item.id, &node, key, value);
            }
            if mode == "TAG" {
                desired.set(&item.id, &node, "VLANID", &vlan);
                desired.set(&item.id, &node, "dot1pData", wan.priority);
            }
            desired.set(
                &item.id,
                &node,
                "Active",
                if wan.enabled { "Yes" } else { "No" },
            );
            let mut bindings = wan
                .bindings
                .iter()
                .map(binding)
                .collect::<Result<BTreeSet<_>, _>>()?;
            for related in device.items.iter().filter(|related| {
                related.binding_intent.is_some() && related.wan_ref == item.wan_ref
            }) {
                for endpoint in &related.binding_intent.as_ref().unwrap().endpoints {
                    bindings.insert(binding(endpoint)?);
                }
            }
            for prefix in ["LAN", "SSID", "SSIDAC"] {
                for n in 1..=4 {
                    let key = format!("{prefix}{n}");
                    desired.set(
                        &item.id,
                        &node,
                        &key,
                        if bindings.contains(&key) { "Yes" } else { "No" },
                    );
                }
            }
            desired.owned.wan.insert(
                item.id.clone(),
                Owned {
                    parent: p,
                    child: c,
                    service,
                    vlan,
                },
            );
        } else if item.binding_intent.is_some() {
            // Native bindings are part of the WAN transaction, never a second write.
        } else {
            return Err("item has no supported native business intent".into());
        }
    }
    for item in device.items.iter().filter(|i| i.binding_intent.is_some()) {
        let service = device
            .items
            .iter()
            .find(|s| s.wan_intent.is_some() && s.wan_ref == item.wan_ref)
            .ok_or("native binding has no matching desired WAN")?;
        desired
            .items
            .insert(item.id.clone(), desired.items[&service.id].clone());
    }
    if full {
        if device.wifi_cleanup == "delete" {
            return Err(
                "native fixed Wi-Fi slots cannot be deleted; select disable or preserve".into(),
            );
        }
        if device.wifi_cleanup == "disable" {
            for (band, radio) in [2, 5].into_iter().enumerate() {
                for slot in 1..=4 {
                    let node = wlan(&EndpointRef::Ssid { radio, slot })?;
                    if !wifi_nodes.contains(&node)
                        && !device.wifi_preserve.contains(&(band as u32 * 4 + slot))
                    {
                        desired.set("wifi_cleanup", &node, "EnableSSID", "0");
                    }
                }
            }
        }
        for (id, old) in desired.owned.wan.clone() {
            if desired_wan.contains(&id) {
                continue;
            }
            if inv.children.contains(&(old.parent, old.child)) {
                let node = format!("Wan_PVC{}_Entry{}", old.parent, old.child);
                if protected(get(inv, &node, "ServiceList"))
                    || get(inv, &node, "ServiceList") != old.service
                    || get(inv, &format!("Wan_PVC{}", old.parent), "VLANID") != old.vlan
                    || inv
                        .children
                        .iter()
                        .any(|(p, c)| *p == old.parent && *c != old.child)
                {
                    return Err(
                        "stale owned WAN changed or shares a parent; refusing removal".into(),
                    );
                }
                desired.removed.insert((old.parent, old.child));
            } else {
                desired.owned.wan.remove(&id);
            }
        }
    }
    Ok(desired)
}

fn differences(desired: &Desired, actual: &Inventory) -> Fields {
    desired
        .fields
        .iter()
        .filter(|(key, value)| actual.fields.get(*key) != Some(*value))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
fn write_script_body(diff: &Fields, removed: &BTreeSet<(u32, u32)>) -> String {
    let mut script = String::from("set -e\n");
    let mut roots = BTreeSet::new();
    let wan_nodes: BTreeSet<_> = diff
        .keys()
        .filter(|(n, _)| n.starts_with("Wan_") && n.contains("_Entry"))
        .map(|(n, _)| n.clone())
        .collect();
    for node in &wan_nodes {
        // Create the service identity before any remaining child attributes.
        if let Some(service) = diff.get(&(node.clone(), "ServiceList".into())) {
            script.push_str(&format!(
                "tcapi set {} ServiceList {} >/dev/null\n",
                quote(node),
                quote(service)
            ));
        }
        script.push_str(&format!("tcapi set {} Active No >/dev/null\n", quote(node)));
    }
    for ((node, key), value) in diff {
        roots.insert(node.split('_').next().unwrap().to_string());
        if key == "Active" {
            continue;
        }
        script.push_str(&format!(
            "tcapi set {} {} {} >/dev/null\n",
            quote(node),
            quote(key),
            quote(value)
        ));
    }
    for node in &wan_nodes {
        if let Some(active) = diff.get(&(node.clone(), "Active".into())) {
            script.push_str(&format!(
                "tcapi set {} Active {} >/dev/null\n",
                quote(node),
                quote(active)
            ));
        } else {
            script.push_str(&format!(
                "tcapi set {} Active Yes >/dev/null\n",
                quote(node)
            ));
        }
    }
    for (p, c) in removed {
        script.push_str(&format!(
            "tcapi set Wan_PVC{p}_Entry{c} Active No >/dev/null\ntcapi commit Wan_PVC{p}_Entry{c} >/dev/null\ntcapi unset Wan_PVC{p}_Entry{c} >/dev/null\ntcapi unset Wan_PVC{p} >/dev/null\n"
        ));
        roots.insert("Wan".into());
    }
    for root in roots {
        if root == "Wan" {
            for node in &wan_nodes {
                script.push_str(&format!("tcapi commit {} >/dev/null\n", quote(node)));
            }
        }
        script.push_str(&format!("tcapi commit {} >/dev/null\n", quote(&root)));
    }
    script.push_str("tcapi save >/dev/null\n");
    script
}

#[cfg(test)]
fn write_script(diff: &Fields, removed: &BTreeSet<(u32, u32)>) -> String {
    let mut script = write_script_body(diff, removed);
    script.push_str("printf 'FTTR_WRITE_COMPLETE\\n'\n");
    script
}

pub(super) fn ownership_path(serial: &str) -> String {
    crate::provisioning::runtime_store::Record::Native.path(serial)
}

fn selection(serial: &str) -> Result<(Selection, String), String> {
    let registry = crate::mqtt_identity::shared().lock().unwrap();
    let targets: Vec<_> = registry
        .snapshots(now())
        .into_iter()
        .filter(|s| s.optical_sn.as_deref() == Some(serial) && s.reason == "ready")
        .collect();
    let [target] = targets.as_slice() else {
        return Err("fresh unique optical/MQTT identity required".into());
    };
    Ok((
        Selection {
            optical_sn: serial.into(),
            onu_id: target.onu_id.ok_or("missing ONU identity")?,
            binding_generation: target.binding_generation.clone(),
        },
        target.mqtt_mac.clone(),
    ))
}

fn load_ownership(serial: &str) -> Result<Ownership, String> {
    match storage::read(ownership_path(serial)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "invalid command ownership journal".into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ownership::default()),
        Err(_) => Err("cannot read command ownership journal".into()),
    }
}

pub(crate) fn has_recoverable_tx(serial: &str) -> bool {
    load_ownership(serial).is_ok_and(|o| o.pending && o.pending_tx.is_some())
}

fn parse_boot_and_inventory(text: &str) -> Result<(Option<String>, bool, Inventory), String> {
    let (boot, rest) = text.split_once('\n').ok_or("missing boot probe frame")?;
    let (boot_id, restart) = boot
        .strip_prefix("BOOT|")
        .and_then(|v| v.split_once('|'))
        .ok_or("invalid boot probe frame")?;
    let inventory = parse(rest)?;
    let boot_id = valid_boot_id(boot_id).then(|| boot_id.to_ascii_lowercase());
    Ok((boot_id, restart == "1", inventory))
}

fn parse_probe_recovery_output(
    text: &str,
) -> Result<(Option<String>, RemoteExecutionState, Inventory), String> {
    let (boot, rest) = text.split_once('\n').ok_or("missing boot probe frame")?;
    let (boot_id, _) = boot
        .strip_prefix("BOOT|")
        .and_then(|v| v.split_once('|'))
        .ok_or("invalid boot probe frame")?;
    let (exec_line, inv_text) = rest
        .split_once('\n')
        .ok_or("missing remote execution status frame")?;
    let remote = parse_exec_status(exec_line)?;
    let inventory = parse(inv_text)?;
    let boot_id = valid_boot_id(boot_id).then(|| boot_id.to_ascii_lowercase());
    Ok((boot_id, remote, inventory))
}

#[derive(Debug, PartialEq, Eq)]
enum WriteReply {
    Complete,
    RemoteBusy,
    BootMismatch(String),
    RemoteFailed,
    Uncertain,
}

fn parse_write_reply(reply: &command_binding::SelectedResult) -> WriteReply {
    if !reply.binding_current {
        return WriteReply::Uncertain;
    }
    if reply.result.output_truncated {
        return WriteReply::Uncertain;
    }
    if reply.result.status == CommandStatus::Timeout {
        return WriteReply::Uncertain;
    }
    let output = reply.result.output.as_deref().unwrap_or("");
    if output.lines().any(|line| line == "FTTR_EXEC_BUSY") {
        return WriteReply::RemoteBusy;
    }
    if let Some(boot) = output
        .lines()
        .find_map(|line| line.strip_prefix("FTTR_EXEC_BOOT_MISMATCH|"))
    {
        return WriteReply::BootMismatch(boot.to_string());
    }
    if reply.result.status == CommandStatus::Executed
        && reply.result.exit_code == Some(0)
        && output.lines().any(|line| line == "FTTR_WRITE_COMPLETE")
    {
        return WriteReply::Complete;
    }
    if reply.result.status == CommandStatus::Executed && reply.result.exit_code.is_some() {
        return WriteReply::RemoteFailed;
    }
    WriteReply::Uncertain
}

fn build_pending_tx(
    device: &Device,
    desired: &Desired,
    request_id: String,
    boot_id: String,
) -> Result<PendingTransaction, String> {
    let mut expected_fields: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut expected_secret_macs = BTreeMap::new();
    for ((node, key), value) in &desired.fields {
        if key == "WPAPSK" {
            expected_secret_macs.insert(node.clone(), runtime_store::secret_mac(node, value)?);
        } else {
            expected_fields
                .entry(node.clone())
                .or_default()
                .insert(key.clone(), value.clone());
        }
    }
    let item_fields = desired
        .items
        .iter()
        .map(|(id, fields)| (id.clone(), fields.iter().cloned().collect()))
        .collect();
    let item_meta = device
        .items
        .iter()
        .map(|item| (item.id.clone(), (item.kind, item.service_type.clone())))
        .collect();
    Ok(PendingTransaction {
        request_id,
        boot_id,
        dispatched_at: now(),
        expected_fields,
        expected_secret_macs,
        removed_wans: desired.removed.iter().copied().collect(),
        item_fields,
        item_meta,
        remote_state: None,
        resume_attempts: 0,
    })
}

fn persist_ownership(path: &str, owned: &Ownership) -> Result<(), String> {
    storage::write_atomic(
        path,
        &serde_json::to_vec(owned).map_err(|_| "serialize command journal")?,
        true,
    )
    .map_err(|_| "persist command journal".to_string())
}

/// Observe current intent without asserting anything about the old writer.
/// No write callback, ownership update, or raw inventory leaves this path.
pub async fn observe(
    device: &Device,
    auth_epoch: u64,
) -> Result<Value, crate::provisioning::command::Failure> {
    let (selection, mac) = selection(&device.optical_sn)?;
    let reply = super::command::Transaction::observe_inventory(&mac, &selection)
        .await
        .map_err(|e| format!("inventory command failed: {e}"))?;
    let text = inventory_reply(reply).map_err(|(reason, _)| reason)?;
    let (boot_id, restart, inventory) = parse_boot_and_inventory(&text)?;
    if crate::omci_auth::serial_epoch(&device.optical_sn) != Some(auth_epoch) {
        return Err("OMCI authentication generation changed".into());
    }
    let owned = load_ownership(&device.optical_sn)?;
    let mut result = observation(device, &inventory, owned);
    if let Some(boot_id) = boot_id {
        result["boot_id"] = json!(boot_id);
        result["restart_available"] = json!(restart);
    }
    Ok(result)
}

#[cfg(feature = "diagnostics")]
pub async fn restart_for_recovery(
    device: &Device,
    boot_id: &str,
) -> Result<Value, crate::provisioning::command::Failure> {
    if !load_ownership(&device.optical_sn)?.pending {
        return Err("native pending record no longer exists".into());
    }
    let (selection, mac) = selection(&device.optical_sn)?;
    let reply = super::command::Transaction::restart_for_recovery(&mac, &selection, boot_id)
        .await
        .map_err(|e| {
            crate::provisioning::command::Failure::uncertain(format!("restart dispatch: {e}"))
        })?;
    if reply.result.exit_code.is_some_and(|code| code != 0) {
        return Err("restart command was rejected by the device".into());
    }
    Ok(
        json!({"restart_reply": if reply.binding_current && reply.result.exit_code == Some(0) { "acknowledged" } else { "unconfirmed" },
        "previous_boot_id":boot_id, "requires_fresh_observation":true}),
    )
}

fn observation(device: &Device, inventory: &Inventory, owned: Ownership) -> Value {
    let native_pending = owned.pending.then_some(owned.operation_id);
    let mut result =
        json!({"inventory_complete":true,"native_pending_operation_id":native_pending});
    match compile(device, inventory, owned, true) {
        Ok(desired) => {
            let items: Vec<_> = device.items.iter().map(|item| {
                let fields = &desired.items[&item.id];
                let matches = fields.iter().all(|k| desired.fields.get(k) == inventory.fields.get(k))
                    && runtime_matches(&desired, inventory, fields);
                // Match/drift are observations, not provisioning item outcomes.
                json!({"id":item.id,"kind":item.kind,"comparison":if matches {"match"} else {"drift"}})
            }).collect();
            let matches = differences(&desired, inventory).is_empty()
                && desired
                    .removed
                    .iter()
                    .all(|pair| !inventory.children.contains(pair))
                && desired
                    .items
                    .values()
                    .all(|fields| runtime_matches(&desired, inventory, fields));
            result["comparison"] = json!(if matches { "match" } else { "drift" });
            result["items"] = json!(items);
        }
        Err(_) => result["comparison"] = json!("unavailable"),
    }
    result
}

pub async fn run(
    device: Device,
    apply: bool,
    full: bool,
    revision: String,
    operation_id: u64,
    auth_epoch: u64,
) -> Result<Value, crate::provisioning::command::Failure> {
    let check_auth = || {
        if crate::omci_auth::serial_epoch(&device.optical_sn) == Some(auth_epoch) {
            Ok(())
        } else {
            Err("OMCI authentication generation changed".to_string())
        }
    };
    check_auth()?;
    let (selection, target_mac) = selection(&device.optical_sn)?;
    // Separate from per-publication transport locks; held for the complete transaction.
    let transaction = super::command::Transaction::acquire(&target_mac)
        .map_err(|e| format!("device transaction admission rejected: {e}"))?;
    let path = ownership_path(&device.optical_sn);
    let owned = load_ownership(&device.optical_sn)?;
    let read_failure = |reason: String| {
        if owned.pending {
            crate::provisioning::command::Failure::uncertain(reason)
        } else {
            reason.into()
        }
    };
    let probe_script = format!("{BOOT_PROBE}\n{INVENTORY}");
    // Inventory is read-only. A timeout does not cancel the remote shell, so
    // only retry this initial read, never a write or its uncertain readback.
    let inventory_text = initial_inventory_with_retry(
        || execute_once(&transaction, &selection, &probe_script),
        check_auth,
        Duration::from_secs(2),
    )
    .await
    .map_err(read_failure)?;
    let (boot_id, _, inventory) =
        parse_boot_and_inventory(&inventory_text).map_err(read_failure)?;
    check_auth().map_err(read_failure)?;
    let request_id = super::command::request_id();
    reconcile(
        Transaction {
            device: &device,
            apply,
            full,
            revision: &revision,
            operation_id,
            binding_generation: &selection.binding_generation,
            request_id: &request_id,
            boot_id: boot_id.as_deref(),
        },
        inventory,
        owned,
        |req_id, script| {
            let selection = &selection;
            let transaction = &transaction;
            async move {
                match command_binding::execute_request_async(
                    transaction,
                    selection,
                    req_id,
                    &script,
                    Duration::from_secs(90),
                )
                .await
                {
                    Ok((reply, publication)) => Ok((parse_write_reply(&reply), publication)),
                    Err((error, publication)) => {
                        Err((format!("command transport failed: {error}"), publication))
                    }
                }
            }
        },
        || {
            let selection = &selection;
            let transaction = &transaction;
            async move { execute(transaction, selection, INVENTORY).await }
        },
        |owned| persist_ownership(&path, owned),
        check_auth,
    )
    .await
}

struct Transaction<'a> {
    device: &'a Device,
    apply: bool,
    full: bool,
    revision: &'a str,
    operation_id: u64,
    binding_generation: &'a str,
    request_id: &'a str,
    boot_id: Option<&'a str>,
}

fn runtime_matches(
    desired: &Desired,
    actual: &Inventory,
    fields: &BTreeSet<(String, String)>,
) -> bool {
    fields.iter().all(|(node, key)| {
        key != "Active"
            || desired
                .fields
                .get(&(node.clone(), key.clone()))
                .is_none_or(|v| v != "Yes")
            || get(actual, node, "__runtime_present") == "1"
    })
}

async fn reconcile<FW, FutW, FR, FutR, P, C>(
    transaction: Transaction<'_>,
    inventory: Inventory,
    owned: Ownership,
    mut execute_write: FW,
    mut execute_read: FR,
    mut persist: P,
    check_auth: C,
) -> Result<Value, crate::provisioning::command::Failure>
where
    FW: FnMut(String, String) -> FutW,
    FutW: std::future::Future<
        Output = Result<(WriteReply, PublicationState), (String, PublicationState)>,
    >,
    FR: FnMut() -> FutR,
    FutR: std::future::Future<Output = Result<String, String>>,
    P: FnMut(&Ownership) -> Result<(), String>,
    C: Fn() -> Result<(), String>,
{
    let Transaction {
        device,
        apply,
        full,
        revision,
        operation_id,
        binding_generation,
        request_id,
        boot_id,
    } = transaction;
    let unresolved = owned.pending;
    let unresolved_operation = unresolved.then(|| json!({"revision":owned.revision,"operation_id":owned.operation_id,"binding_generation":owned.binding_generation}));
    let mut desired = compile(device, &inventory, owned, full).map_err(|reason| {
        if unresolved {
            crate::provisioning::command::Failure::uncertain(reason)
        } else {
            reason.into()
        }
    })?;
    if !unresolved {
        desired.owned.revision = revision.into();
        desired.owned.operation_id = operation_id;
        desired.owned.binding_generation = binding_generation.into();
    }
    let mut diff = differences(&desired, &inventory);
    // Matching persistent fields do not prove the firmware applied a WAN.
    for owned in desired.owned.wan.values() {
        if desired.removed.contains(&(owned.parent, owned.child)) {
            continue;
        }
        let node = format!("Wan_PVC{}_Entry{}", owned.parent, owned.child);
        if desired
            .fields
            .get(&(node.clone(), "Active".into()))
            .is_some_and(|v| v == "Yes")
            && get(&inventory, &node, "__runtime_present") != "1"
        {
            diff.insert((node, "Active".into()), "Yes".into());
        }
    }
    for ((node, key), value) in &desired.fields {
        if key == "Active"
            && diff.keys().any(|(changed, _)| {
                changed == node || changed == node.split("_Entry").next().unwrap()
            })
        {
            diff.insert((node.clone(), key.clone()), value.clone());
        }
    }
    let changed = !diff.is_empty() || !desired.removed.is_empty();
    let mut actual = inventory;
    // Legacy pending has neither the original intent nor remote terminal proof.
    // Even a no-diff snapshot (or a plan) cannot settle that old transaction.
    let mut uncertain = unresolved;
    let mut write_dispatched = false;
    let mut terminal_received = false;
    let mut publication_state = None;
    let mut remote_execution_state = None;
    let mut dispatched_request_id = None;
    if apply && changed && !unresolved {
        let boot_id = boot_id
            .filter(|id| valid_boot_id(id))
            .ok_or("valid ONU boot ID required before native write")?;
        let pre_write_wan = desired.owned.wan.clone();
        let pre_write_schema = desired.owned.schema_version;
        let inner = write_script_body(&diff, &desired.removed);
        let wrapped = wrapped_write_script(&inner, request_id, boot_id, operation_id)?;
        let tx = build_pending_tx(device, &desired, request_id.into(), boot_id.into())?;
        desired.owned.schema_version = 2;
        desired.owned.pending = true;
        desired.owned.pending_tx = Some(tx);
        persist(&desired.owned)?;
        check_auth()?;
        dispatched_request_id = Some(request_id.to_string());
        match execute_write(request_id.to_string(), wrapped).await {
            Ok((reply, publication)) => {
                write_dispatched = true;
                publication_state = Some(publication.as_str());
                match reply {
                    WriteReply::Complete => {
                        terminal_received = true;
                        remote_execution_state = Some("finished");
                        uncertain = false;
                    }
                    WriteReply::RemoteFailed => {
                        terminal_received = true;
                        remote_execution_state = Some("finished");
                        uncertain = true;
                    }
                    WriteReply::RemoteBusy => {
                        remote_execution_state = Some("running");
                        uncertain = true;
                    }
                    WriteReply::BootMismatch(_) => {
                        remote_execution_state = Some("rebooted");
                        uncertain = true;
                    }
                    WriteReply::Uncertain => {
                        remote_execution_state = Some("unknown");
                        uncertain = true;
                    }
                }
            }
            Err((reason, PublicationState::NotEnqueued)) => {
                desired.owned.pending = false;
                desired.owned.pending_tx = None;
                desired.owned.wan = pre_write_wan;
                desired.owned.schema_version = pre_write_schema;
                persist(&desired.owned)
                    .map_err(crate::provisioning::command::Failure::uncertain)?;
                return Err(reason.into());
            }
            Err((_, publication)) => {
                write_dispatched = true;
                publication_state = Some(publication.as_str());
                remote_execution_state = Some("unknown");
                uncertain = true;
            }
        }
        check_auth().map_err(crate::provisioning::command::Failure::uncertain)?;
        match execute_read().await.and_then(|s| parse(&s)) {
            Ok(readback) => actual = readback,
            Err(_) => uncertain = true,
        }
    }
    let remaining = differences(&desired, &actual);
    let removed_verified = desired
        .removed
        .iter()
        .all(|pair| !actual.children.contains(pair));
    let verified = remaining.is_empty()
        && removed_verified
        && !uncertain
        && desired
            .items
            .values()
            .all(|fields| runtime_matches(&desired, &actual, fields));
    if apply && verified {
        desired
            .owned
            .wan
            .retain(|_, owned| !desired.removed.contains(&(owned.parent, owned.child)));
        desired.owned.pending = false;
        desired.owned.pending_tx = None;
        persist(&desired.owned).map_err(|reason| {
            if write_dispatched {
                crate::provisioning::command::Failure::uncertain(reason)
            } else {
                reason.into()
            }
        })?;
    }
    let result_for = |id: &str, fields: &BTreeSet<(String, String)>| {
        let matches = fields
            .iter()
            .all(|k| desired.fields.get(k) == actual.fields.get(k))
            && runtime_matches(&desired, &actual, fields);
        let state = if uncertain {
            "unknown"
        } else if matches {
            "verified"
        } else {
            "drift"
        };
        json!({"id":id,"state":state,"updated_at":now(),"reason":if uncertain {"write outcome uncertain; fresh inventory required; no automatic replay"} else if matches {"native tcapi readback matches desired state"} else if apply {"native tcapi readback differs after apply"} else {"read-only plan: changes required"},"changes":fields.iter().filter(|k| diff.contains_key(*k)).map(|(node,key)|json!({"name":format!("{node}.{key}"),"desired":if key=="WPAPSK" {"<redacted>"} else {desired.fields.get(&(node.clone(),key.clone())).map(String::as_str).unwrap_or("")}})).collect::<Vec<_>>()})
    };
    let items: Vec<_> = device
        .items
        .iter()
        .map(|item| {
            let mut result = result_for(&item.id, desired.items.get(&item.id).unwrap());
            result["kind"] = json!(item.kind);
            result["service_type"] = json!(item.service_type);
            result
        })
        .collect();
    let cleanup = desired
        .items
        .get("wifi_cleanup")
        .map(|fields| result_for("wifi_cleanup", fields));
    Ok(
        json!({"items":items,"wifi_lifecycle":cleanup.map(|v|json!({"native_cleanup":v})).unwrap_or(json!({})),"wan_lifecycle":{"final_inventory":{"state":if uncertain {"unknown"} else if verified {"verified"} else {"drift"},"reason":"native inventory; management/shared parents protected"}},"native_ownership":desired.owned.wan,"write_performed":write_dispatched,"readback_verified":verified,"write_attempted":write_dispatched,"terminal_received":terminal_received,"write_verified":verified && write_dispatched,"publication_state":publication_state,"remote_execution_state":remote_execution_state,"request_id":dispatched_request_id,"unresolved_operation":unresolved_operation}),
    )
}

struct IntentComparison {
    remaining_diff: Fields,
    remaining_removed: BTreeSet<(u32, u32)>,
    drifted_secrets: BTreeSet<String>,
}

fn verify_against_intent(
    tx: &PendingTransaction,
    owned_wan: &BTreeMap<String, Owned>,
    actual: &Inventory,
) -> Result<IntentComparison, String> {
    let mut remaining_diff = Fields::new();
    for (node, keys) in &tx.expected_fields {
        for (key, value) in keys {
            if actual.fields.get(&(node.clone(), key.clone())) != Some(value) {
                remaining_diff.insert((node.clone(), key.clone()), value.clone());
            }
        }
    }
    let removed_set: BTreeSet<_> = tx.removed_wans.iter().copied().collect();
    for owned in owned_wan.values() {
        if removed_set.contains(&(owned.parent, owned.child)) {
            continue;
        }
        let node = format!("Wan_PVC{}_Entry{}", owned.parent, owned.child);
        if tx
            .expected_fields
            .get(&node)
            .and_then(|fields| fields.get("Active"))
            .is_some_and(|v| v == "Yes")
            && get(actual, &node, "__runtime_present") != "1"
        {
            remaining_diff.insert((node, "Active".into()), "Yes".into());
        }
    }
    let remaining_removed: BTreeSet<_> = removed_set
        .into_iter()
        .filter(|pair| actual.children.contains(pair))
        .collect();
    let mut drifted_secrets = BTreeSet::new();
    for (node, expected_mac) in &tx.expected_secret_macs {
        let actual_secret = get(actual, node, "WPAPSK");
        let actual_mac = runtime_store::secret_mac(node, actual_secret)?;
        if &actual_mac != expected_mac {
            drifted_secrets.insert(node.clone());
        }
    }
    Ok(IntentComparison {
        remaining_diff,
        remaining_removed,
        drifted_secrets,
    })
}

fn resolve_remaining_diff(
    device: &Device,
    tx: &PendingTransaction,
    owned_wan: &BTreeMap<String, Owned>,
    actual: &Inventory,
    comparison: IntentComparison,
) -> Result<(Fields, BTreeSet<(u32, u32)>), String> {
    let mut diff = comparison.remaining_diff;
    let removed = comparison.remaining_removed;
    for node in comparison.drifted_secrets {
        let expected_mac = &tx.expected_secret_macs[&node];
        let mut matched_key = None;
        for item in &device.items {
            if let Some(wlan_intent) = &item.wlan_intent {
                if wlan(&wlan_intent.endpoint).ok().as_deref() == Some(node.as_str()) {
                    if let Some(key) = &wlan_intent.key {
                        if runtime_store::secret_mac(&node, key)? == *expected_mac {
                            matched_key = Some(key.clone());
                            break;
                        }
                    }
                }
            }
        }
        let Some(key) = matched_key else {
            return Err("original Wi-Fi credential unavailable in current configuration; manual intervention required".into());
        };
        diff.insert((node, "WPAPSK".into()), key);
    }
    let tx_removed: BTreeSet<_> = tx.removed_wans.iter().copied().collect();
    for owned in owned_wan.values() {
        if tx_removed.contains(&(owned.parent, owned.child)) {
            continue;
        }
        let parent_node = format!("Wan_PVC{}", owned.parent);
        let node = format!("{parent_node}_Entry{}", owned.child);
        if actual.children.contains(&(owned.parent, owned.child)) {
            if protected(get(actual, &node, "ServiceList"))
                || get(actual, &node, "ServiceList") != owned.service
                || get(actual, &parent_node, "VLANID") != owned.vlan
                || actual
                    .children
                    .iter()
                    .any(|(p, c)| *p == owned.parent && *c != owned.child)
            {
                return Err(
                    "owned WAN identity changed or shares a parent; manual intervention required"
                        .into(),
                );
            }
        } else if actual.parents.contains(&owned.parent) {
            return Err("target WAN parent is no longer free; manual intervention required".into());
        }
    }
    for (p, c) in &removed {
        let parent_node = format!("Wan_PVC{p}");
        let node = format!("{parent_node}_Entry{c}");
        if protected(get(actual, &node, "ServiceList"))
            || !owned_wan.values().any(|o| {
                o.parent == *p
                    && o.child == *c
                    && get(actual, &node, "ServiceList") == o.service
                    && get(actual, &parent_node, "VLANID") == o.vlan
            })
            || actual
                .children
                .iter()
                .any(|(other_p, other_c)| other_p == p && other_c != c)
        {
            return Err(
                "stale owned WAN changed or shares a parent; manual removal refused".into(),
            );
        }
    }
    for (node, keys) in &tx.expected_fields {
        if let Some(active) = keys.get("Active") {
            let parent = node.split("_Entry").next().unwrap_or(node);
            if diff
                .keys()
                .any(|(changed, _)| changed == node || changed == parent)
            {
                diff.insert((node.clone(), "Active".into()), active.clone());
            }
        }
    }
    Ok((diff, removed))
}

fn evaluate_current_config(
    device: &Device,
    actual: &Inventory,
    owned: &Ownership,
) -> (bool, Vec<Value>, Value) {
    match compile(device, actual, owned.clone(), true) {
        Ok(desired) => {
            let diff = differences(&desired, actual);
            let items: Vec<_> = device
                .items
                .iter()
                .map(|item| {
                    let fields = &desired.items[&item.id];
                    let matches = fields.iter().all(|k| desired.fields.get(k) == actual.fields.get(k))
                        && runtime_matches(&desired, actual, fields);
                    json!({
                        "id": item.id,
                        "kind": item.kind,
                        "service_type": item.service_type,
                        "state": if matches { "verified" } else { "drift" },
                        "updated_at": now(),
                        "reason": if matches { "native tcapi readback matches desired state" } else { "settled prior write; configuration drift remains for latest revision" },
                        "changes": fields.iter().filter(|k| diff.contains_key(*k)).map(|(node, key)| {
                            json!({"name": format!("{node}.{key}"), "desired": if key == "WPAPSK" { "<redacted>" } else { desired.fields.get(&(node.clone(), key.clone())).map(String::as_str).unwrap_or("") }})
                        }).collect::<Vec<_>>(),
                    })
                })
                .collect();
            let cleanup = desired.items.get("wifi_cleanup").map(|fields| {
                let matches = fields.iter().all(|k| desired.fields.get(k) == actual.fields.get(k))
                    && runtime_matches(&desired, actual, fields);
                json!({"native_cleanup": {"id": "wifi_cleanup", "state": if matches { "verified" } else { "drift" }, "updated_at": now(), "reason": if matches { "native tcapi readback matches desired state" } else { "configuration drift remains" }}})
            }).unwrap_or_else(|| json!({}));
            let all_match = diff.is_empty()
                && desired
                    .removed
                    .iter()
                    .all(|pair| !actual.children.contains(pair))
                && desired
                    .items
                    .values()
                    .all(|fields| runtime_matches(&desired, actual, fields));
            (all_match, items, cleanup)
        }
        Err(reason) => {
            let items = device
                .items
                .iter()
                .map(|item| {
                    json!({
                        "id": item.id,
                        "kind": item.kind,
                        "service_type": item.service_type,
                        "state": "blocked",
                        "updated_at": now(),
                        "reason": reason,
                    })
                })
                .collect();
            (false, items, json!({}))
        }
    }
}

fn unresolved_intent_items(tx: &PendingTransaction, state: &str, reason: &str) -> Vec<Value> {
    tx.item_meta
        .iter()
        .map(|(id, (kind, service_type))| {
            json!({
                "id": id,
                "kind": kind,
                "service_type": service_type,
                "state": state,
                "updated_at": now(),
                "reason": reason,
            })
        })
        .collect()
}

pub async fn recover_pending(
    device: Device,
    apply: bool,
    operation_id: u64,
    auth_epoch: u64,
) -> Result<Value, crate::provisioning::command::Failure> {
    let check_auth = || {
        if crate::omci_auth::serial_epoch(&device.optical_sn) == Some(auth_epoch) {
            Ok(())
        } else {
            Err("OMCI authentication generation changed".to_string())
        }
    };
    check_auth()?;
    let (selection, target_mac) = selection(&device.optical_sn)?;
    let transaction = super::command::Transaction::acquire_for_recovery(&target_mac)
        .map_err(|e| format!("device transaction admission rejected: {e}"))?;
    let path = ownership_path(&device.optical_sn);
    let owned = load_ownership(&device.optical_sn)?;
    let read_failure = |reason: String| {
        if owned.pending {
            crate::provisioning::command::Failure::uncertain(reason)
        } else {
            reason.into()
        }
    };
    let probe_text = initial_inventory_with_retry(
        || async {
            match &owned.pending_tx {
                Some(tx) => {
                    transaction
                        .probe_recovery(&selection, &tx.request_id, &tx.boot_id)
                        .await
                }
                None => {
                    let probe = format!("{BOOT_PROBE}\nEXEC_STATUS|unknown||\n{INVENTORY}");
                    execute_once(&transaction, &selection, &probe).await
                }
            }
        },
        check_auth,
        Duration::from_secs(2),
    )
    .await
    .map_err(read_failure)?;
    let (current_boot_id, remote, inventory) =
        parse_probe_recovery_output(&probe_text).map_err(read_failure)?;
    check_auth().map_err(read_failure)?;
    let next_req = super::command::request_id();
    recover_pending_core(
        &device,
        apply,
        operation_id,
        owned,
        current_boot_id,
        remote,
        inventory,
        &next_req,
        |req_id, script| {
            let selection = &selection;
            let transaction = &transaction;
            async move {
                match command_binding::execute_request_async(
                    transaction,
                    selection,
                    req_id,
                    &script,
                    Duration::from_secs(90),
                )
                .await
                {
                    Ok((reply, publication)) => Ok((parse_write_reply(&reply), publication)),
                    Err((error, publication)) => {
                        Err((format!("command transport failed: {error}"), publication))
                    }
                }
            }
        },
        || {
            let selection = &selection;
            let transaction = &transaction;
            async move { execute(transaction, selection, INVENTORY).await }
        },
        |owned| persist_ownership(&path, owned),
        check_auth,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn recover_pending_core<FW, FutW, FR, FutR, P, C>(
    device: &Device,
    apply: bool,
    operation_id: u64,
    mut owned: Ownership,
    current_boot_id: Option<String>,
    remote: RemoteExecutionState,
    inventory: Inventory,
    next_request_id: &str,
    mut execute_write: FW,
    mut execute_read: FR,
    mut persist: P,
    check_auth: C,
) -> Result<Value, crate::provisioning::command::Failure>
where
    FW: FnMut(String, String) -> FutW,
    FutW: std::future::Future<
        Output = Result<(WriteReply, PublicationState), (String, PublicationState)>,
    >,
    FR: FnMut() -> FutR,
    FutR: std::future::Future<Output = Result<String, String>>,
    P: FnMut(&Ownership) -> Result<(), String>,
    C: Fn() -> Result<(), String>,
{
    if !owned.pending {
        let (all_match, items, cleanup) = evaluate_current_config(device, &inventory, &owned);
        return Ok(json!({
            "settled": true,
            "settled_operation_id": owned.operation_id,
            "settled_matches_current": all_match,
            "recovery_outcome": "already_settled",
            "recovery_state": "none",
            "remote_execution_state": "finished",
            "boot_id": current_boot_id,
            "write_performed": false,
            "readback_verified": all_match,
            "write_attempted": false,
            "terminal_received": true,
            "write_verified": false,
            "items": items,
            "wifi_lifecycle": cleanup,
            "native_ownership": owned.wan,
        }));
    }
    let old_operation_id = owned.operation_id;
    let Some(mut tx) = owned.pending_tx.clone() else {
        let mut obs = observation(device, &inventory, owned);
        obs["settled"] = json!(false);
        obs["recovery_outcome"] = json!("legacy_pending");
        obs["recovery_state"] = json!("manual_required");
        obs["remote_execution_state"] = json!("legacy");
        obs["boot_id"] = json!(current_boot_id);
        obs["write_performed"] = json!(false);
        obs["readback_verified"] = json!(false);
        obs["manual_action"] = json!("legacy pending record lacks original intent and remote receipt; confirm old writer termination independently");
        return Ok(obs);
    };
    if !remote.confirmed_ended() {
        let is_running = remote == RemoteExecutionState::Running;
        let recovery_state = if is_running {
            "checking_execution"
        } else {
            "manual_required"
        };
        let recovery_outcome = if is_running {
            "still_running"
        } else {
            "manual_required"
        };
        let manual_action = if is_running {
            "remote writer is still running on the ONU; waiting before verification"
        } else {
            "remote execution termination could not be proven on the current ONU boot"
        };
        let items = unresolved_intent_items(&tx, "unknown", manual_action);
        return Ok(json!({
            "settled": false,
            "recovery_outcome": recovery_outcome,
            "recovery_state": recovery_state,
            "remote_execution_state": remote.as_str(),
            "boot_id": current_boot_id,
            "write_performed": false,
            "readback_verified": false,
            "write_attempted": false,
            "terminal_received": false,
            "write_verified": false,
            "manual_action": manual_action,
            "items": items,
            "native_ownership": owned.wan,
        }));
    }

    let comparison = verify_against_intent(&tx, &owned.wan, &inventory)?;
    if comparison.remaining_diff.is_empty()
        && comparison.remaining_removed.is_empty()
        && comparison.drifted_secrets.is_empty()
    {
        owned
            .wan
            .retain(|_, w| !tx.removed_wans.contains(&(w.parent, w.child)));
        owned.pending = false;
        owned.pending_tx = None;
        persist(&owned).map_err(crate::provisioning::command::Failure::uncertain)?;
        let (all_match, items, cleanup) = evaluate_current_config(device, &inventory, &owned);
        return Ok(json!({
            "settled": true,
            "settled_operation_id": old_operation_id,
            "settled_revision": owned.revision,
            "settled_matches_current": all_match,
            "recovery_outcome": "settled",
            "recovery_state": "none",
            "remote_execution_state": remote.as_str(),
            "remote_exit_code": remote.exit_code(),
            "boot_id": current_boot_id,
            "write_performed": false,
            "readback_verified": true,
            "write_attempted": false,
            "terminal_received": true,
            "write_verified": false,
            "items": items,
            "wifi_lifecycle": cleanup,
            "native_ownership": owned.wan,
        }));
    }

    let (remaining_diff, remaining_removed) =
        match resolve_remaining_diff(device, &tx, &owned.wan, &inventory, comparison) {
            Ok(resolved) => resolved,
            Err(conflict) => {
                let items = unresolved_intent_items(&tx, "unknown", &conflict);
                return Ok(json!({
                    "settled": false,
                    "recovery_outcome": "manual_required",
                    "recovery_state": "manual_required",
                    "remote_execution_state": remote.as_str(),
                    "remote_exit_code": remote.exit_code(),
                    "boot_id": current_boot_id,
                    "write_performed": false,
                    "readback_verified": false,
                    "manual_action": conflict,
                    "items": items,
                    "native_ownership": owned.wan,
                }));
            }
        };

    if !apply {
        let msg = "old execution ended with remaining drift; explicit apply or recover required";
        let items = unresolved_intent_items(&tx, "unknown", msg);
        return Ok(json!({
            "settled": false,
            "recovery_outcome": "drift_awaiting_apply",
            "recovery_state": "manual_required",
            "remote_execution_state": remote.as_str(),
            "remote_exit_code": remote.exit_code(),
            "boot_id": current_boot_id,
            "write_performed": false,
            "readback_verified": false,
            "manual_action": msg,
            "items": items,
            "native_ownership": owned.wan,
        }));
    }

    if tx.resume_attempts >= 2 {
        let msg = "recovery write budget exhausted; manual inspection required";
        let items = unresolved_intent_items(&tx, "unknown", msg);
        return Ok(json!({
            "settled": false,
            "recovery_outcome": "manual_required",
            "recovery_state": "manual_required",
            "remote_execution_state": remote.as_str(),
            "remote_exit_code": remote.exit_code(),
            "boot_id": current_boot_id,
            "write_performed": false,
            "readback_verified": false,
            "manual_action": msg,
            "items": items,
            "native_ownership": owned.wan,
        }));
    }

    let cur_boot = current_boot_id
        .clone()
        .filter(|id| valid_boot_id(id))
        .ok_or("valid ONU boot ID required before recovery write")?;
    let prev_request_id = tx.request_id.clone();
    let prev_boot_id = tx.boot_id.clone();
    let wrapped = wrapped_write_script(
        &write_script_body(&remaining_diff, &remaining_removed),
        next_request_id,
        &cur_boot,
        operation_id,
    )?;
    tx.request_id = next_request_id.to_string();
    tx.boot_id = cur_boot;
    tx.resume_attempts += 1;
    tx.remote_state = Some(remote.as_str().to_string());
    owned.operation_id = operation_id;
    owned.pending_tx = Some(tx.clone());
    persist(&owned).map_err(crate::provisioning::command::Failure::uncertain)?;
    check_auth().map_err(crate::provisioning::command::Failure::uncertain)?;

    let mut terminal_received = false;
    let publication_state;
    let mut write_ok = false;
    match execute_write(next_request_id.to_string(), wrapped).await {
        Ok((reply, publication)) => {
            publication_state = Some(publication.as_str());
            match reply {
                WriteReply::Complete => {
                    terminal_received = true;
                    write_ok = true;
                }
                WriteReply::RemoteFailed => {
                    terminal_received = true;
                }
                WriteReply::RemoteBusy | WriteReply::BootMismatch(_) | WriteReply::Uncertain => {}
            }
        }
        Err((reason, PublicationState::NotEnqueued)) => {
            tx.request_id = prev_request_id;
            tx.boot_id = prev_boot_id;
            owned.operation_id = old_operation_id;
            owned.pending_tx = Some(tx);
            persist(&owned).map_err(crate::provisioning::command::Failure::uncertain)?;
            return Err(crate::provisioning::command::Failure::uncertain(reason));
        }
        Err((_, publication)) => {
            publication_state = Some(publication.as_str());
        }
    }

    check_auth().map_err(crate::provisioning::command::Failure::uncertain)?;
    let readback = match execute_read().await.and_then(|s| parse(&s)) {
        Ok(readback) => readback,
        Err(_) => {
            let msg = "recovery write readback unavailable; retaining unresolved intent";
            let items = unresolved_intent_items(&tx, "unknown", msg);
            return Ok(json!({
                "settled": false,
                "recovery_outcome": "still_uncertain",
                "recovery_state": "retry_scheduled",
                "remote_execution_state": if write_ok { "finished" } else { "unknown" },
                "boot_id": current_boot_id,
                "write_performed": true,
                "readback_verified": false,
                "write_attempted": true,
                "terminal_received": terminal_received,
                "write_verified": false,
                "publication_state": publication_state,
                "request_id": next_request_id,
                "items": items,
                "native_ownership": owned.wan,
            }));
        }
    };

    let post_check = verify_against_intent(&tx, &owned.wan, &readback)?;
    if write_ok
        && post_check.remaining_diff.is_empty()
        && post_check.remaining_removed.is_empty()
        && post_check.drifted_secrets.is_empty()
    {
        owned
            .wan
            .retain(|_, w| !tx.removed_wans.contains(&(w.parent, w.child)));
        owned.pending = false;
        owned.pending_tx = None;
        persist(&owned).map_err(crate::provisioning::command::Failure::uncertain)?;
        let (all_match, items, cleanup) = evaluate_current_config(device, &readback, &owned);
        return Ok(json!({
            "settled": true,
            "settled_operation_id": old_operation_id,
            "settled_revision": owned.revision,
            "settled_matches_current": all_match,
            "recovery_outcome": "resumed_settled",
            "recovery_state": "none",
            "remote_execution_state": "finished",
            "remote_exit_code": 0,
            "boot_id": current_boot_id,
            "write_performed": true,
            "readback_verified": true,
            "write_attempted": true,
            "terminal_received": true,
            "write_verified": true,
            "publication_state": publication_state,
            "request_id": next_request_id,
            "items": items,
            "wifi_lifecycle": cleanup,
            "native_ownership": owned.wan,
        }));
    }

    let msg = "remaining differences persist after recovery write";
    let items = unresolved_intent_items(&tx, "unknown", msg);
    Ok(json!({
        "settled": false,
        "recovery_outcome": "still_uncertain",
        "recovery_state": "retry_scheduled",
        "remote_execution_state": if write_ok { "finished" } else { "unknown" },
        "boot_id": current_boot_id,
        "write_performed": true,
        "readback_verified": false,
        "write_attempted": true,
        "terminal_received": terminal_received,
        "write_verified": false,
        "publication_state": publication_state,
        "request_id": next_request_id,
        "items": items,
        "native_ownership": owned.wan,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_wifi_text(ssid: &str) -> String {
        let mut text = String::new();
        for radio in ["WLan", "WLan11ac"] {
            for (node, keys) in std::iter::once((format!("{radio}_Common"), "APOn BssidNum")).chain(
                (0..4).map(|slot| {
                    (
                    format!("{radio}_Entry{slot}"),
                    "SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256",
                )
                }),
            ) {
                for key in keys.split_whitespace() {
                    let value = match key {
                        "BssidNum" => "4",
                        "SSID" => ssid,
                        _ => "1",
                    };
                    text.push_str(&format!(
                        "V|{node}|{key}|{}\n",
                        value
                            .bytes()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    ));
                }
            }
        }
        text.push_str("FTTR_INVENTORY_COMPLETE\n");
        text
    }

    #[test]
    fn inventory_requires_every_field_and_rejects_non_ascii_hex_without_panicking() {
        let text = complete_wifi_text("test");
        assert!(parse(&text).is_ok());
        let lines: Vec<_> = text.lines().collect();
        for skip in 0..lines.len() {
            let incomplete = lines
                .iter()
                .enumerate()
                .filter(|(n, _)| *n != skip)
                .map(|(_, line)| *line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse(&incomplete).is_err(), "accepted missing line {skip}");
        }
        for hex in ["a中", "é", "zz", "0"] {
            assert!(parse(&text.replacen("|74657374", &format!("|{hex}"), 1)).is_err());
        }
        assert!(parse(&(text.clone() + "P|0\n")).is_err());
        assert!(parse(&format!("P|0\n{text}")).is_err());
        assert!(parse(&format!("C|0|0\n{text}")).is_err());
        assert!(parse(&format!("V|Wan_PVC7|VLANID|31\n{text}")).is_err());
    }

    #[test]
    fn recovery_comparison_does_not_export_inventory_or_clear_pending() {
        let (mut device, _) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "fixture-network".into(),
            enabled: None,
            broadcast: None,
            security: None,
            key: Some("fixture-desired-credential".into()),
        });
        let mut inventory = parse(&complete_wifi_text("fixture-network")).unwrap();
        let key = wlan(&device.items[0].wlan_intent.as_ref().unwrap().endpoint).unwrap();
        let field = (key, "WPAPSK".into());
        let owned = Ownership {
            pending: true,
            operation_id: 9,
            ..Ownership::default()
        };
        for (secret, comparison) in [
            ("fixture-actual-credential", "drift"),
            ("fixture-desired-credential", "match"),
        ] {
            inventory.fields.insert(field.clone(), secret.into());
            let result = observation(&device, &inventory, owned.clone());
            assert_eq!(result["native_pending_operation_id"], 9);
            assert_eq!(result["comparison"], comparison);
            assert_eq!(result["items"][0]["comparison"], comparison);
            let output = result.to_string();
            for private in ["credential", "fixture-network", "WPAPSK", "verified"] {
                assert!(!output.contains(private));
            }
        }
    }

    #[tokio::test]
    async fn legacy_pending_never_settles_or_dispatches_even_when_current_desired_matches() {
        let (device, _) = fixture();
        let desired = compile(&device, &Inventory::default(), Ownership::default(), true).unwrap();
        let mut inventory = Inventory {
            fields: desired.fields.clone(),
            ..Inventory::default()
        };
        inventory.parents.insert(0);
        inventory.children.insert((0, 0));
        inventory.fields.insert(
            ("Wan_PVC0_Entry0".into(), "__runtime_present".into()),
            "1".into(),
        );
        let mut owned = desired.owned;
        owned.pending = true;
        owned.operation_id = 9;
        owned.revision = "original-revision".into();
        owned.binding_generation = "original-binding".into();
        for apply in [false, true] {
            for drift in [false, true] {
                let mut actual = inventory.clone();
                if drift {
                    actual
                        .fields
                        .remove(&("Wan_PVC0_Entry0".into(), "LAN2".into()));
                }
                let result = reconcile(
                    Transaction {
                        device: &device,
                        apply,
                        full: true,
                        revision: "new-revision",
                        operation_id: 42,
                        binding_generation: "new-binding",
                        request_id: "req_legacy",
                        boot_id: Some("73d5ff2a-4f34-409a-9577-166c32c0a83b"),
                    },
                    actual,
                    owned.clone(),
                    |_, _| async { panic!("pending must not dispatch") },
                    || async { panic!("pending must not readback") },
                    |_| panic!("pending journal must remain byte-for-byte unchanged"),
                    || Ok(()),
                )
                .await
                .unwrap();
                assert_eq!(result["items"][0]["state"], "unknown");
                assert_eq!(result["readback_verified"], false);
                assert_eq!(result["write_performed"], false);
                assert_eq!(result["unresolved_operation"]["operation_id"], 9);
                assert_eq!(
                    result["unresolved_operation"]["revision"],
                    "original-revision"
                );
            }
        }
    }

    #[tokio::test]
    async fn native_writes_require_durable_intent_and_terminal_plus_complete_readback() {
        use std::cell::RefCell;
        let (mut device, _) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "wanted".into(),
            enabled: None,
            broadcast: None,
            security: None,
            key: None,
        });
        let boot_id = "73d5ff2a-4f34-409a-9577-166c32c0a83b";
        let transaction = || Transaction {
            device: &device,
            apply: true,
            full: true,
            revision: "new",
            operation_id: 42,
            binding_generation: "binding",
            request_id: "req_42",
            boot_id: Some(boot_id),
        };
        let result = reconcile(
            transaction(),
            parse(&complete_wifi_text("old")).unwrap(),
            Ownership::default(),
            |_, _| async { panic!("failed intent must not dispatch") },
            || async { panic!("failed intent must not readback") },
            |_| Err("injected save failure".into()),
            || Ok(()),
        )
        .await;
        assert!(result.is_err());
        for terminal in [true, false] {
            let saves = RefCell::new(Vec::new());
            let writes = RefCell::new(0);
            let reads = RefCell::new(0);
            let result = reconcile(
                transaction(),
                parse(&complete_wifi_text("old")).unwrap(),
                Ownership::default(),
                |_, _| {
                    *writes.borrow_mut() += 1;
                    async move {
                        if terminal {
                            Ok((WriteReply::Complete, PublicationState::FlushConfirmed))
                        } else {
                            Err(("timeout".into(), PublicationState::FlushConfirmed))
                        }
                    }
                },
                || {
                    *reads.borrow_mut() += 1;
                    async move { Ok(complete_wifi_text("wanted")) }
                },
                |owned| {
                    saves
                        .borrow_mut()
                        .push((owned.pending, owned.pending_tx.is_some()));
                    Ok(())
                },
                || Ok(()),
            )
            .await
            .unwrap();
            assert_eq!(result["write_performed"], true);
            assert_eq!(result["readback_verified"], terminal);
            assert_eq!(
                *saves.borrow(),
                if terminal {
                    vec![(true, true), (false, false)]
                } else {
                    vec![(true, true)]
                }
            );
            assert_eq!(
                result["items"][0]["state"],
                if terminal { "verified" } else { "unknown" }
            );
        }
        // Errors after dispatch (including a failed durable settlement) must
        // retain uncertainty instead of becoming an ordinary blocked read.
        for auth_revoked in [false, true] {
            let calls = RefCell::new(0);
            let auth_checks = RefCell::new(0);
            let saves = RefCell::new(0);
            let failure = reconcile(
                transaction(),
                parse(&complete_wifi_text("old")).unwrap(),
                Ownership::default(),
                |_, _| {
                    *calls.borrow_mut() += 1;
                    async { Ok((WriteReply::Complete, PublicationState::FlushConfirmed)) }
                },
                || {
                    *calls.borrow_mut() += 1;
                    async { Ok(complete_wifi_text("wanted")) }
                },
                |_| {
                    *saves.borrow_mut() += 1;
                    if *saves.borrow() == 2 {
                        Err("injected settlement barrier failure".into())
                    } else {
                        Ok(())
                    }
                },
                || {
                    *auth_checks.borrow_mut() += 1;
                    if auth_revoked && *auth_checks.borrow() == 2 {
                        Err("authorization revoked".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .await
            .unwrap_err();
            assert!(failure.uncertain);
            assert_eq!(*calls.borrow(), if auth_revoked { 1 } else { 2 });
        }
    }

    #[tokio::test]
    async fn initial_inventory_retries_only_timeout_and_checks_auth() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async {
                if calls.get() == 1 {
                    Err(("timeout".into(), true))
                } else {
                    Ok("inventory".into())
                }
            }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Ok(()), Duration::ZERO)
                .await
                .unwrap(),
            "inventory"
        );
        assert_eq!(calls.get(), 2);

        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async { Err(("identity changed".into(), false)) }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Ok(()), Duration::ZERO)
                .await
                .unwrap_err(),
            "identity changed"
        );
        assert_eq!(calls.get(), 1);

        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async { Err(("timeout".into(), true)) }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Err("auth changed".into()), Duration::ZERO)
                .await
                .unwrap_err(),
            "auth changed"
        );
        assert_eq!(calls.get(), 1);
    }
    fn fixture() -> (Device, Inventory) {
        let item = Item {
            id: "wan".into(),
            kind: ItemKind::DeviceService,
            service_type: "iptv".into(),
            parameters: vec![],
            guard: None,
            blocked: None,
            wan_ref: Some("wan".into()),
            auto_instance: None,
            wlan_intent: None,
            binding_intent: None,
            wan_intent: Some(WanIntent {
                services: vec!["IPTV".into()],
                vlan: VlanMode::Tagged(88),
                priority: 4,
                mode: ConnectionMode::Bridge,
                bindings: vec![EndpointRef::Lan(2)],
                internet: false,
                enabled: true,
            }),
        };
        (
            Device {
                id: "child".into(),
                optical_sn: "H3CT12345678".into(),
                cwmp_id: "pending:H3CT12345678".into(),
                omci_adapter: "hl202".into(),
                bootstrap_adapter: "none".into(),
                provisioning_adapter: "hl202-mqtt".into(),
                resolved_contract: None,
                connection_request: ConnectionRequest::default(),
                items: vec![item],
                wifi_cleanup: "preserve".into(),
                wifi_preserve: BTreeSet::new(),
            },
            Inventory::default(),
        )
    }
    #[test]
    fn removal_keeps_ownership_until_absence_is_verified() {
        let (mut device, mut inv) = fixture();
        let first = compile(&device, &inv, Ownership::default(), true).unwrap();
        inv.parents.insert(0);
        inv.children.insert((0, 0));
        inv.fields.insert(
            ("Wan_PVC0_Entry0".into(), "ServiceList".into()),
            "IPTV".into(),
        );
        inv.fields
            .insert(("Wan_PVC0".into(), "VLANID".into()), "88".into());
        device.items.clear();
        let deleting = compile(&device, &inv, first.owned, true).unwrap();
        assert!(deleting.removed.contains(&(0, 0)));
        assert!(deleting.owned.wan.contains_key("wan"));
        let mut pending = deleting.owned;
        pending.pending = true;
        let recovered = compile(&device, &inv, pending, true).unwrap();
        assert!(recovered.owned.pending);
        assert!(recovered.removed.contains(&(0, 0)));
        inv.children.clear();
        let absent = compile(&device, &inv, recovered.owned, true).unwrap();
        assert!(absent.owned.wan.is_empty());
    }
    #[test]
    fn logical_radio_cleanup_preserves_requested_slots() {
        let (mut device, inv) = fixture();
        device.wifi_cleanup = "disable".into();
        device.wifi_preserve.insert(6);
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "test-network".into(),
            enabled: Some(true),
            broadcast: Some(true),
            security: None,
            key: None,
        });
        let desired = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(
            desired.fields[&("WLan_Entry0".into(), "EnableSSID".into())],
            "1"
        );
        assert_eq!(
            desired.fields[&("WLan11ac_Entry0".into(), "EnableSSID".into())],
            "0"
        );
        assert!(!desired
            .fields
            .contains_key(&("WLan11ac_Entry1".into(), "EnableSSID".into())));
        assert_eq!(desired.items["wifi_cleanup"].len(), 6);
    }
    #[test]
    fn native_plan_preserves_management_and_uses_free_parent() {
        let (device, mut inv) = fixture();
        inv.parents.insert(0);
        inv.children.insert((0, 0));
        inv.fields.insert(
            ("Wan_PVC0_Entry0".into(), "ServiceList".into()),
            "TR069_INTERNET".into(),
        );
        let desired = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(desired.owned.wan["wan"].parent, 1);
        assert!(desired
            .fields
            .keys()
            .all(|(node, _)| !node.starts_with("Wan_PVC0")));
        assert_eq!(
            desired.fields[&("Wan_PVC1_Entry0".into(), "LAN2".into())],
            "Yes"
        );
        assert_eq!(
            desired.fields[&("Wan_PVC1_Entry0".into(), "LAN1".into())],
            "No"
        );
        assert!(differences(&desired, &inv).len() > 10);
        assert!(inv.fields.len() == 1); // planning never changes observed state
    }
    #[test]
    fn shared_and_reassigned_owned_wans_fail_closed() {
        let (device, mut inv) = fixture();
        inv.parents.insert(1);
        inv.children.insert((1, 0));
        inv.children.insert((1, 1));
        inv.fields.insert(
            ("Wan_PVC1_Entry0".into(), "ServiceList".into()),
            "IPTV".into(),
        );
        inv.fields.insert(
            ("Wan_PVC1_Entry0".into(), "WanMode".into()),
            "Bridge".into(),
        );
        inv.fields
            .insert(("Wan_PVC1".into(), "VLANID".into()), "88".into());
        assert!(compile(&device, &inv, Ownership::default(), true).is_err());
        inv.children.remove(&(1, 1));
        let mut owned = Ownership::default();
        owned.wan.insert(
            "wan".into(),
            Owned {
                parent: 1,
                child: 0,
                service: "OTHER".into(),
                vlan: "88".into(),
            },
        );
        assert!(compile(&device, &inv, owned, true).is_err());
    }
    #[test]
    fn wpa3_transition_resets_required_pmf_when_returning_to_wpa2() {
        let (mut device, inv) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 5, slot: 2 },
            ssid: "test".into(),
            enabled: Some(true),
            broadcast: Some(true),
            security: Some(WifiSecurity::Wpa3Sae),
            key: Some("test-only-key".into()),
        });
        let pure = compile(&device, &inv, Ownership::default(), true).unwrap();
        let field = |key: &str| ("WLan11ac_Entry1".into(), key.into());
        assert_eq!(pure.fields[&field("AuthMode")], "WPA3PSK");
        assert_eq!(pure.fields[&field("PMFMFPR")], "1");
        device.items[0].wlan_intent.as_mut().unwrap().security = Some(WifiSecurity::Wpa2PskWpa3Sae);
        let mixed = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(mixed.fields[&field("AuthMode")], "WPA2PSKWPA3PSK");
        assert_eq!(mixed.fields[&field("PMFMFPC")], "1");
        assert_eq!(mixed.fields[&field("PMFMFPR")], "0");
        device.items[0].wlan_intent.as_mut().unwrap().security = Some(WifiSecurity::Wpa2Psk);
        let observed = Inventory {
            fields: pure.fields,
            ..inv
        };
        let restored = compile(&device, &observed, Ownership::default(), true).unwrap();
        let diff = differences(&restored, &observed);
        assert_eq!(diff[&field("PMFMFPC")], "0");
        assert_eq!(diff[&field("PMFMFPR")], "0");
    }
    #[test]
    fn backend_defaults_and_explicit_override_follow_compiled_capability() {
        let (mut device, _) = fixture();
        assert_eq!(device.backend(), "command");
        device.provisioning_adapter = "hl202-cu".into();
        assert_eq!(device.backend(), "acs");
        device.provisioning_adapter = "hwtc-hw".into();
        device.omci_adapter = "hwtc".into();
        assert_eq!(device.backend(), "acs");
    }
    #[test]
    fn native_endpoints_are_not_cwmp_instances() {
        assert_eq!(
            wlan(&EndpointRef::Ssid { radio: 5, slot: 1 }).unwrap(),
            "WLan11ac_Entry0"
        );
        assert!(wlan(&EndpointRef::Ssid { radio: 5, slot: 5 }).is_err());
        assert_eq!(
            binding(&EndpointRef::Ssid { radio: 5, slot: 4 }).unwrap(),
            "SSIDAC4"
        );
    }
    #[test]
    fn framing_and_duplicates_fail_closed() {
        assert!(parse("P|0\nP|0\nFTTR_INVENTORY_COMPLETE\n").is_err());
        assert!(parse("C|0|0\n").is_err());
        assert!(protected("TR069_INTERNET"));
        assert!(!protected("INTERNET"));
    }
    #[test]
    fn secrets_and_shell_metacharacters_are_data() {
        let value = "a'$(touch /tmp/unwanted)";
        let diff = BTreeMap::from([(("WLan_Entry0".into(), "WPAPSK".into()), value.into())]);
        let script = write_script(&diff, &BTreeSet::new());
        assert!(script.contains("'a'\\''$(touch /tmp/unwanted)'"));
        assert!(script.find("tcapi commit").unwrap() < script.find("tcapi save").unwrap());
    }

    #[tokio::test]
    async fn not_enqueued_write_rolls_back_pending_while_flushed_timeout_retains_verifiable_intent()
    {
        use std::cell::RefCell;
        let (mut device, _) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "wanted".into(),
            enabled: Some(true),
            broadcast: None,
            security: None,
            key: Some("secret-pass".into()),
        });
        let boot_id = "73d5ff2a-4f34-409a-9577-166c32c0a83b";
        let persisted = RefCell::new(None::<Ownership>);
        let err = reconcile(
            Transaction {
                device: &device,
                apply: true,
                full: true,
                revision: "rev1",
                operation_id: 10,
                binding_generation: "b1",
                request_id: "req_10",
                boot_id: Some(boot_id),
            },
            parse(&complete_wifi_text("old")).unwrap(),
            Ownership::default(),
            |_, _| async { Err(("connect refused".into(), PublicationState::NotEnqueued)) },
            || async { panic!("not_enqueued must not read back") },
            |owned| {
                *persisted.borrow_mut() = Some(owned.clone());
                Ok(())
            },
            || Ok(()),
        )
        .await
        .unwrap_err();
        assert!(!err.uncertain);
        let rolled_back = persisted.borrow().clone().unwrap();
        assert!(!rolled_back.pending);
        assert!(rolled_back.pending_tx.is_none());

        // Now simulate FlushConfirmed timeout: pending_tx is preserved without plaintext WPAPSK.
        let reply = reconcile(
            Transaction {
                device: &device,
                apply: true,
                full: true,
                revision: "rev1",
                operation_id: 11,
                binding_generation: "b1",
                request_id: "req_11",
                boot_id: Some(boot_id),
            },
            parse(&complete_wifi_text("old")).unwrap(),
            Ownership::default(),
            |_, _| async { Err(("timeout".into(), PublicationState::FlushConfirmed)) },
            || async { Ok(complete_wifi_text("old")) },
            |owned| {
                *persisted.borrow_mut() = Some(owned.clone());
                Ok(())
            },
            || Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(reply["items"][0]["state"], "unknown");
        assert_eq!(reply["publication_state"], "flush_confirmed");
        let saved = persisted.borrow().clone().unwrap();
        assert!(saved.pending);
        let tx = saved.pending_tx.unwrap();
        assert_eq!(tx.request_id, "req_11");
        assert_eq!(tx.boot_id, boot_id);
        assert!(tx.expected_secret_macs.contains_key("WLan_Entry0"));
        assert!(!serde_json::to_string(&tx).unwrap().contains("secret-pass"));
    }

    #[tokio::test]
    async fn recovery_handles_running_finished_partial_and_missing_receipts_without_blind_replay() {
        use std::cell::RefCell;
        let (mut device, _) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "wanted".into(),
            enabled: Some(true),
            broadcast: None,
            security: None,
            key: Some("secret-pass".into()),
        });
        let boot_id = "73d5ff2a-4f34-409a-9577-166c32c0a83b";
        let desired = compile(&device, &Inventory::default(), Ownership::default(), true).unwrap();
        let tx = build_pending_tx(&device, &desired, "req_20".into(), boot_id.into()).unwrap();
        let mut pending_owned = desired.owned.clone();
        pending_owned.schema_version = 2;
        pending_owned.pending = true;
        pending_owned.operation_id = 20;
        pending_owned.revision = "rev1".into();
        pending_owned.pending_tx = Some(tx);

        // 1. Still running: blocks writes, stays unresolved.
        let running = recover_pending_core(
            &device,
            true,
            21,
            pending_owned.clone(),
            Some(boot_id.into()),
            RemoteExecutionState::Running,
            parse(&complete_wifi_text("wanted")).unwrap(),
            "req_21",
            |_, _| async { panic!("running remote writer must block new writes") },
            || async { panic!("no readback when not writing") },
            |_| panic!("must not settle while running"),
            || Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(running["settled"], false);
        assert_eq!(running["recovery_outcome"], "still_running");
        assert_eq!(running["recovery_state"], "checking_execution");

        // 2. Missing receipt on same boot: stays manual_required, never assumes never-ran.
        let missing = recover_pending_core(
            &device,
            true,
            22,
            pending_owned.clone(),
            Some(boot_id.into()),
            RemoteExecutionState::Missing,
            parse(&complete_wifi_text("old")).unwrap(),
            "req_22",
            |_, _| async { panic!("missing receipt must not replay") },
            || async { panic!("no readback") },
            |_| panic!("must not clear pending"),
            || Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(missing["settled"], false);
        assert_eq!(missing["recovery_outcome"], "manual_required");

        // 3. Finished(0) and matching inventory (including secret MAC): settles with 0 writes even when apply=false!
        let mut matched_inv = parse(&complete_wifi_text("wanted")).unwrap();
        matched_inv.fields.insert(
            ("WLan_Entry0".into(), "WPAPSK".into()),
            "secret-pass".into(),
        );
        let saved = RefCell::new(None::<Ownership>);
        let settled = recover_pending_core(
            &device,
            false,
            23,
            pending_owned.clone(),
            Some(boot_id.into()),
            RemoteExecutionState::Finished(0),
            matched_inv,
            "req_23",
            |_, _| async { panic!("matched intent must perform 0 writes") },
            || async { panic!("no second readback needed") },
            |owned| {
                *saved.borrow_mut() = Some(owned.clone());
                Ok(())
            },
            || Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(settled["settled"], true);
        assert_eq!(settled["recovery_outcome"], "settled");
        assert_eq!(settled["write_performed"], false);
        assert_eq!(settled["readback_verified"], true);
        assert!(!saved.borrow().as_ref().unwrap().pending);

        // 4. Partial write (SSID written, WPAPSK still old) after Finished(1):
        // resumes ONLY WPAPSK with a new request_id and settles!
        let partial_inv = parse(&complete_wifi_text("wanted")).unwrap();
        let dispatched_script = RefCell::new(String::new());
        let saves = RefCell::new(Vec::new());
        let mut updated_inv = partial_inv.clone();
        updated_inv.fields.insert(
            ("WLan_Entry0".into(), "WPAPSK".into()),
            "secret-pass".into(),
        );
        let resumed = recover_pending_core(
            &device,
            true,
            24,
            pending_owned,
            Some(boot_id.into()),
            RemoteExecutionState::Finished(1),
            partial_inv,
            "req_24",
            |req_id, script| {
                assert_eq!(req_id, "req_24");
                *dispatched_script.borrow_mut() = script;
                async { Ok((WriteReply::Complete, PublicationState::FlushConfirmed)) }
            },
            || async { Ok(complete_wifi_text_with_secret("wanted", "secret-pass")) },
            |owned| {
                saves.borrow_mut().push((
                    owned.pending,
                    owned.pending_tx.as_ref().map(|t| t.request_id.clone()),
                ));
                Ok(())
            },
            || Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(resumed["settled"], true);
        assert_eq!(resumed["recovery_outcome"], "resumed_settled");
        assert_eq!(resumed["write_performed"], true);
        assert_eq!(resumed["readback_verified"], true);
        assert!(dispatched_script.borrow().contains("WPAPSK"));
        assert!(!dispatched_script.borrow().contains("'SSID'"));
        assert_eq!(
            *saves.borrow(),
            vec![(true, Some("req_24".into())), (false, None)]
        );
    }

    fn complete_wifi_text_with_secret(ssid: &str, secret: &str) -> String {
        let mut text = String::new();
        for radio in ["WLan", "WLan11ac"] {
            for (node, keys) in std::iter::once((format!("{radio}_Common"), "APOn BssidNum")).chain(
                (0..4).map(|slot| {
                    (
                    format!("{radio}_Entry{slot}"),
                    "SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256",
                )
                }),
            ) {
                for key in keys.split_whitespace() {
                    let value = match key {
                        "BssidNum" => "4",
                        "SSID" => ssid,
                        "WPAPSK" if node == "WLan_Entry0" => secret,
                        _ => "1",
                    };
                    text.push_str(&format!(
                        "V|{node}|{key}|{}\n",
                        value
                            .bytes()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    ));
                }
            }
        }
        text.push_str("FTTR_INVENTORY_COMPLETE\n");
        text
    }
}
