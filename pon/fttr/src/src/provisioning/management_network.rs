//! Host management resources. Discovery is read-only; only an authorized revision mutates UCI.
use super::{config::Section, model::Management, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(test)]
use std::time::Duration;
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
#[path = "management_command.rs"]
mod command;
use command::run;
#[path = "management_network/verification.rs"]
mod verification;
#[cfg(test)]
use verification::dhcp_range_matches;
use verification::{verify_bridge, verify_dhcp, verify_firewall, verify_network};

fn journal_path() -> String {
    super::runtime_store::Record::Management.path("")
}
type Sections = BTreeMap<String, Section>;
use fttr_api::NetworkPhase as Phase;

#[derive(Clone, Serialize, Deserialize)]
pub struct Desired {
    pub management: Management,
    pub enabled: bool,
    pub automatic: bool,
    pub address: String,
    pub prefix: u8,
    pub dhcp: bool,
    pub start: u32,
    pub limit: u32,
    pub lease: String,
    pub acs_enabled: bool,
    pub port: u16,
    pub path: String,
    username: String,
    password: String,
    #[serde(default)]
    pub management_gateway: bool,
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
}
impl Desired {
    pub(super) fn parse(s: &Section, acs: Option<&Section>) -> Result<Self, String> {
        let (ip, prefix) = cidr(s.get("address"))?;
        let mut vlans = Vec::new();
        for value in s.options.get("vlan").into_iter().flatten() {
            if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
                return Err("management VLAN must be an integer in 1..4094".into());
            }
            let vlan = value
                .parse::<u32>()
                .ok()
                .filter(|v| (1..=4094).contains(v))
                .ok_or("management VLAN must be in 1..4094")?;
            if vlans.contains(&vlan) {
                return Err("duplicate management VLAN".into());
            }
            vlans.push(vlan);
        }
        let vlan = *vlans
            .first()
            .ok_or("management VLAN list must not be empty")?;
        let mode = s.get("mode");
        if !matches!(mode, "" | "manual" | "auto") {
            return Err("invalid management application mode".into());
        }
        let get = |key: &str, fallback: &str| {
            acs.map(|a| a.get(key))
                .filter(|v| !v.is_empty())
                .unwrap_or(fallback)
                .to_string()
        };
        let username = get("username", "");
        let password = get("password", "");
        if username.is_empty() != password.is_empty() {
            return Err(
                "ACS username and password must either both be set or both be empty".into(),
            );
        }
        let port = get("port", "7547")
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or("invalid ACS port")?;
        let path = get("path", "/cwmp");
        if !path.starts_with('/')
            || path
                .bytes()
                .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
            || path.contains(['?', '#'])
        {
            return Err("invalid CWMP path".into());
        }
        let number = |key: &str, fallback: u32| -> Result<u32, String> {
            if s.get(key).is_empty() {
                Ok(fallback)
            } else {
                s.number(key, u32::MAX)
            }
        };
        let start = number("dhcp_start", 100)?;
        let limit = number("dhcp_limit", 100)?;
        let hosts = (1u64 << (32 - prefix)) - 1;
        let host = u32::from(ip) as u64 & hosts;
        let end = u64::from(start) + u64::from(limit);
        if s.get("dhcp_enabled") != "0"
            && (start == 0 || limit == 0 || end > hosts || (host >= start as u64 && host < end))
        {
            return Err(
                "DHCP pool is outside the subnet or includes the management address".into(),
            );
        }
        let lease = if s.get("dhcp_leasetime").is_empty() {
            "12h"
        } else {
            s.get("dhcp_leasetime")
        }
        .to_string();
        if !lease
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b's' | b'm' | b'h' | b'd'))
            || !lease.bytes().any(|c| c.is_ascii_digit())
        {
            return Err("invalid management DHCP lease time".into());
        }
        let flag = |key: &str, fallback: bool| -> Result<bool, String> {
            match s.get(key) {
                "" => Ok(fallback),
                "1" => Ok(true),
                "0" => Ok(false),
                _ => Err(format!("invalid management flag {key}")),
            }
        };
        let priority = number("priority", 0)?;
        if priority > 7 {
            return Err("invalid management priority".into());
        }
        Ok(Self {
            management: Management {
                vlans,
                vlan,
                priority,
                acs_url: format!("http://{ip}:{port}{path}"),
            },
            enabled: flag("enabled", true)?,
            automatic: mode == "auto",
            address: ip.to_string(),
            prefix,
            dhcp: flag("dhcp_enabled", true)?,
            start,
            limit,
            lease,
            acs_enabled: get("enabled", "1") == "1",
            port,
            path,
            username,
            password,
            management_gateway: false,
            devices: BTreeMap::new(),
        })
    }
    fn version(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
        format!("{hash:016x}")
    }
    fn public(&self) -> Value {
        json!({"enabled":self.enabled,"mode":if self.automatic {"auto"} else {"manual"},"vlan":self.management.vlans,"priority":self.management.priority,"address":format!("{}/{}",self.address,self.prefix),"acs_url":self.management.acs_url,"port":self.port,"path":self.path,"dhcp_enabled":self.dhcp,"management_gateway":self.management_gateway})
    }
}
fn cidr(value: &str) -> Result<(Ipv4Addr, u8), String> {
    let (ip, prefix) = value
        .split_once('/')
        .ok_or("management address requires an IPv4 prefix")?;
    let ip: Ipv4Addr = ip.parse().map_err(|_| "invalid management IPv4 address")?;
    let prefix: u8 = prefix.parse().map_err(|_| "invalid management prefix")?;
    if !(8..=30).contains(&prefix) || ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return Err("invalid management subnet".into());
    }
    let mask = u32::MAX << (32 - prefix);
    let host = u32::from(ip) & !mask;
    if host == 0 || host == !mask {
        return Err("management address is a network or broadcast address".into());
    }
    Ok((ip, prefix))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Journal {
    authorized: Option<Desired>,
    applied: Option<Desired>,
    previous: Option<Desired>,
    cleanup_pending: bool,
    pending: bool,
    epoch: u64,
    applied_epoch: u64,
}
struct Coordinator {
    journal: Journal,
    // Only cache a snapshot after all durability barriers have succeeded.
    saved: Option<Vec<u8>>,
    requires_apply: bool,
    busy: bool,
    state: Phase,
    error: Option<String>,
    plan: Value,
    next_retry: u64,
    attempts: u32,
    ready: bool,
    last_discovery: u64,
}
static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();
fn coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| {
        let (journal, error) = match storage::read(journal_path()) { Ok(bytes) => match serde_json::from_slice(&bytes) { Ok(j) => (j, None), Err(_) => (Journal::default(), Some("management journal is invalid or uses the old scalar VLAN format; explicitly apply the VLAN list configuration".into())) }, Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Journal::default(), None), Err(_) => (Journal::default(), Some("management journal cannot be read".into())) };
        Mutex::new(Coordinator { journal, saved:None, requires_apply:error.is_some(), busy:false, state:Phase::Discovering, error, plan:json!([]), next_retry:0, attempts:0, ready:false, last_discovery:0 })
    })
}
fn persist(c: &mut Coordinator) -> Result<(), String> {
    persist_journal(&c.journal, &mut c.saved, |bytes| {
        storage::write_atomic(&journal_path(), bytes, true)
    })
}
fn persist_journal(
    journal: &Journal,
    saved: &mut Option<Vec<u8>>,
    write: impl FnOnce(&[u8]) -> std::io::Result<()>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(journal).map_err(|_| "cannot encode management journal")?;
    if saved.as_deref() != Some(bytes.as_slice()) {
        // A failed rename/directory sync can leave new bytes on disk. Forget the
        // old snapshot too, so an in-memory rollback cannot skip its next commit.
        *saved = None;
        write(&bytes).map_err(|_| "cannot persist management authorization")?;
        *saved = Some(bytes);
    }
    Ok(())
}
pub(super) fn active() -> Option<Management> {
    let c = coordinator().lock().unwrap();
    c.ready
        .then(|| {
            c.journal
                .applied
                .as_ref()
                .filter(|d| d.enabled && d.acs_enabled)
                .map(|d| d.management.clone())
        })
        .flatten()
}
impl Coordinator {
    fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
        let version = if ready {
            self.journal
                .applied
                .as_ref()
                .map(|d| format!("{}-{}", d.version(), self.journal.applied_epoch))
                .unwrap_or_default()
        } else {
            String::new()
        };
        super::health_watch::network(version);
    }
}
pub(super) fn matches_desired(d: &Desired) -> bool {
    let c = coordinator().lock().unwrap();
    c.ready
        && c.journal
            .applied
            .as_ref()
            .is_some_and(|a| resource_version(a) == resource_version(d))
}
pub(super) fn grant(id: &str, sn: &str) -> Option<(u64, bool)> {
    let c = coordinator().lock().unwrap();
    if !c.ready {
        return None;
    }
    let d = c.journal.applied.as_ref()?;
    if !d.enabled || !d.acs_enabled || d.devices.get(id).is_none_or(|known| known != sn) {
        return None;
    }
    Some((c.journal.applied_epoch, d.automatic))
}
pub(crate) fn credentials(epoch: u64) -> Option<crate::devices::CwmpCredentials> {
    let c = coordinator().lock().unwrap();
    if !c.ready || c.journal.applied_epoch != epoch {
        return None;
    }
    let d = c.journal.applied.as_ref()?;
    Some(crate::devices::CwmpCredentials {
        username: d.username.clone(),
        password: d.password.clone(),
    })
}
pub(super) fn omci_grant(sn: &str) -> Option<(String, Management, String, u64, bool)> {
    let c = coordinator().lock().unwrap();
    if !c.ready {
        return None;
    }
    let d = c.journal.applied.as_ref()?;
    if !d.enabled || !d.acs_enabled || !d.username.is_empty() {
        return None;
    }
    let id = d.devices.iter().find(|(_, known)| *known == sn)?.0.clone();
    Some((
        id,
        d.management.clone(),
        format!("{}-{}", d.version(), c.journal.applied_epoch),
        c.journal.applied_epoch,
        d.automatic,
    ))
}
pub(super) fn view(desired: Option<&Desired>) -> Value {
    let c = coordinator().lock().unwrap();
    json!({"state":c.state,"error":c.error,"inflight":c.busy,"plan":c.plan,"next_retry_at":c.next_retry,"attempts":c.attempts,"ready":c.ready,"desired":desired.map(Desired::public),"desired_version":desired.map(Desired::version),"applied":c.journal.applied.as_ref().map(Desired::public),"applied_version":c.journal.applied.as_ref().map(Desired::version),"previous":c.journal.previous.as_ref().map(Desired::public)})
}
pub(super) fn request(desired: Desired, apply: bool) -> Result<Value, String> {
    start(desired, apply, true)
}
fn start(desired: Desired, apply: bool, fresh_grant: bool) -> Result<Value, String> {
    let mut c = coordinator().lock().unwrap();
    if c.busy {
        return Err("management operation already active".into());
    }
    if apply && c.requires_apply && !fresh_grant {
        return Err("management journal requires an explicit apply".into());
    }
    if apply {
        let before = c.journal.clone();
        let changed = c
            .journal
            .authorized
            .as_ref()
            .is_none_or(|a| a.version() != desired.version());
        if let Some(applied) = &c.journal.applied {
            if desired.enabled
                && c.journal.previous.is_some()
                && resource_version(applied) != resource_version(&desired)
            {
                return Err("finish the current management migration before applying another network revision".into());
            }
            if desired.enabled
                && applied.enabled
                && endpoint_changed(applied, &desired)
                && c.journal.previous.is_none()
            {
                if (applied.address != desired.address || applied.prefix != desired.prefix)
                    && subnets_overlap(applied, &desired)
                {
                    return Err(
                        "staged management address migration requires a non-overlapping new subnet"
                            .into(),
                    );
                }
                c.journal.previous = Some(applied.clone());
            }
        }
        if fresh_grant || changed {
            c.journal.epoch = c.journal.epoch.saturating_add(1);
            c.attempts = 0;
        }
        c.journal.authorized = Some(desired.clone());
        if fresh_grant || changed || !c.ready || c.journal.pending {
            c.journal.pending = true;
            if let Err(e) = persist(&mut c) {
                c.journal = before;
                return Err(e);
            }
        }
        if fresh_grant || changed {
            c.set_ready(false);
        }
        c.requires_apply = false;
    }
    c.state = Phase::Discovering;
    if !c.requires_apply {
        c.error = None;
    }
    finish_spawn(&mut c, spawn(move || execute(desired, apply)))?;
    Ok(json!({"started":true,"mode":if apply {"apply"} else {"plan"}}))
}
fn spawn(work: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    if crate::runtime::is_stopping() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "service is stopping",
        ));
    }
    crate::runtime::spawn_service("management network", async move {
        if let Err(error) = crate::runtime::spawn_blocking(work).await {
            let mut c = coordinator().lock().unwrap();
            c.busy = false;
            c.state = Phase::RetryWait;
            c.error = Some(format!("management worker failed: {error}"));
            c.next_retry = now() + 15;
        }
        super::controller::wake();
    });
    Ok(())
}

fn finish_spawn<T>(c: &mut Coordinator, result: std::io::Result<T>) -> Result<(), String> {
    match result {
        Ok(_) => {
            c.busy = true;
            Ok(())
        }
        Err(error) => {
            let error = format!("cannot start management worker: {error}");
            c.busy = false;
            c.state = Phase::RetryWait;
            c.error = Some(error.clone());
            c.next_retry = now() + 15;
            Err(error)
        }
    }
}
pub(super) fn tick(desired: Option<&Desired>, cwmp_business_ready: bool) {
    let Some(desired) = desired else {
        let mut c = coordinator().lock().unwrap();
        if c.busy || now() < c.next_retry || now().saturating_sub(c.last_discovery) < 15 {
            return;
        }
        if let Some(applied) = c.journal.applied.clone() {
            c.last_discovery = now();
            let _ = finish_spawn(&mut c, spawn(move || restore(applied)));
        }
        return;
    };
    let time = now();
    let mut c = coordinator().lock().unwrap();
    if c.busy || time < c.next_retry || time.saturating_sub(c.last_discovery) < 15 {
        return;
    }
    c.last_discovery = time;
    if c.journal.cleanup_pending
        || (c.ready
            && c.journal.previous.is_some()
            && c.journal.applied.as_ref().is_some_and(|a| {
                !a.enabled
                    || !a.acs_enabled
                    || (cwmp_business_ready
                        && a.devices.iter().all(|(id, sn)| {
                            super::bootstrap::is_ready_for_management(
                                id,
                                sn,
                                &a.management.acs_url,
                                c.journal.applied_epoch,
                            )
                        }))
            }))
    {
        if let Some(applied) = c.journal.applied.clone() {
            c.journal.cleanup_pending = true;
            if let Err(e) = persist(&mut c) {
                c.error = Some(e);
                return;
            }
            c.state = Phase::CleaningPrevious;
            let _ = finish_spawn(&mut c, spawn(move || cleanup_previous(applied)));
            return;
        }
    }
    let apply = !c.requires_apply && application_authorized(&c.journal, desired);
    if !apply && !c.ready {
        if let Some(applied) = c.journal.applied.clone() {
            let _ = finish_spawn(&mut c, spawn(move || restore(applied)));
            return;
        }
    }
    drop(c);
    let _ = start(desired.clone(), apply, false);
}
fn restore(d: Desired) {
    let previous = coordinator().lock().unwrap().journal.previous.clone();
    let result = verify_network(&d, previous.as_ref())
        .and_then(|_| configure_listener(&d, previous.as_ref()));
    let mut c = coordinator().lock().unwrap();
    c.busy = false;
    match result.and_then(|_| persist(&mut c)) {
        Ok(()) => {
            c.set_ready(true);
            c.state = if c.journal.previous.is_some() {
                Phase::WaitingDevices
            } else {
                Phase::Ready
            };
            c.error = None;
        }
        Err(e) => {
            c.set_ready(false);
            c.state = Phase::AwaitingApply;
            c.error = Some(e);
            c.next_retry = now() + 15;
        }
    }
}
fn application_authorized(j: &Journal, d: &Desired) -> bool {
    d.automatic
        || (j.pending
            && j.authorized
                .as_ref()
                .is_some_and(|a| a.version() == d.version()))
}
fn resource_version(d: &Desired) -> String {
    let mut d = d.clone();
    d.devices.clear();
    d.automatic = false;
    d.version()
}
fn endpoint_changed(a: &Desired, b: &Desired) -> bool {
    a.address != b.address
        || a.prefix != b.prefix
        || a.port != b.port
        || a.path != b.path
        || a.username != b.username
        || a.password != b.password
}
fn subnets_overlap(a: &Desired, b: &Desired) -> bool {
    let mask = u32::MAX << (32 - a.prefix.min(b.prefix));
    u32::from(a.address.parse::<Ipv4Addr>().unwrap()) & mask
        == u32::from(b.address.parse::<Ipv4Addr>().unwrap()) & mask
}
fn phase(state: Phase) {
    coordinator().lock().unwrap().state = state;
}
fn parse_export(text: &str, package: &str) -> Result<Sections, String> {
    let mut sections = Sections::new();
    let mut id = None;
    let mut counts = BTreeMap::<String, usize>::new();
    for line in text.lines() {
        let w = crate::config::words(line)?;
        if w.is_empty() {
            continue;
        }
        match w[0].as_str() {
            "package" => (),
            "config" if w.len() == 2 || w.len() == 3 => {
                let n = counts.entry(w[1].clone()).or_default();
                let name = if w.len() == 3 {
                    w[2].clone()
                } else {
                    format!("@{}[{}]", w[1], n)
                };
                *n += 1;
                let key = format!("{package}.{name}");
                sections.insert(
                    key.clone(),
                    Section {
                        kind: w[1].clone(),
                        ..Default::default()
                    },
                );
                id = Some(key);
            }
            "option" | "list" if w.len() == 3 => {
                let section = sections
                    .get_mut(id.as_ref().ok_or("option before section")?)
                    .ok_or("missing section")?;
                if w[0] == "option" {
                    section.options.insert(w[1].clone(), vec![w[2].clone()]);
                } else {
                    section
                        .options
                        .entry(w[1].clone())
                        .or_default()
                        .push(w[2].clone());
                }
            }
            _ => return Err("invalid UCI discovery output".into()),
        }
    }
    Ok(sections)
}
fn discover() -> Result<Sections, String> {
    let mut uci = match openwrt_uci::Uci::new() {
        Ok(u) => u,
        Err(e) => return Err(format!("cannot initialize UCI context: {e}")),
    };
    let mut all = Sections::new();
    for package in ["network", "dhcp", "firewall"] {
        let text = uci
            .export_package(package)
            .map_err(|e| format!("cannot export {package}: {e}"))?;
        all.extend(parse_export(&text, package)?);
    }
    Ok(all)
}
fn expected(d: &Desired) -> Sections {
    let mut out = Sections::new();
    let mut add = |id: &str, kind: &str, fields: Vec<(&str, String)>| {
        out.insert(
            id.into(),
            Section {
                kind: kind.into(),
                options: fields
                    .into_iter()
                    .map(|(k, v)| (k.into(), vec![v]))
                    .collect(),
            },
        );
    };
    for vlan in &d.management.vlans {
        add(
            &format!("network.fttr_tr069_vlan_{vlan}"),
            "device",
            vec![
                ("type", "8021q".into()),
                ("ifname", "br-lan".into()),
                ("name", format!("br-lan.{vlan}")),
                ("vid", vlan.to_string()),
                ("egress_qos_mapping", format!("0:{}", d.management.priority)),
            ],
        );
    }
    add(
        "network.fttr_tr069_bridge",
        "device",
        vec![
            ("type", "bridge".into()),
            ("name", "br-tr069".into()),
            ("vlan_filtering", "0".into()),
            ("bridge_empty", "1".into()),
        ],
    );
    add(
        "network.fttr_tr069",
        "interface",
        vec![
            ("device", "br-tr069".into()),
            ("proto", "static".into()),
            ("ipaddr", format!("{}/{}", d.address, d.prefix)),
            ("auto", if d.enabled { "1" } else { "0" }.into()),
            ("delegate", "0".into()),
        ],
    );
    add(
        "dhcp.fttr_tr069",
        "dhcp",
        vec![
            ("interface", "fttr_tr069".into()),
            ("start", d.start.to_string()),
            ("limit", d.limit.to_string()),
            ("leasetime", d.lease.clone()),
            ("ignore", if d.enabled && d.dhcp { "0" } else { "1" }.into()),
            (
                "dhcpv4",
                if d.enabled && d.dhcp {
                    "server"
                } else {
                    "disabled"
                }
                .into(),
            ),
            ("dhcpv6", "disabled".into()),
            ("ra", "disabled".into()),
            ("ndp", "disabled".into()),
        ],
    );
    out.get_mut("network.fttr_tr069_bridge")
        .unwrap()
        .options
        .insert(
            "ports".into(),
            d.management
                .vlans
                .iter()
                .map(|v| format!("br-lan.{v}"))
                .collect(),
        );
    let dhcp_options = if d.management_gateway {
        vec![format!("3,{}", d.address), "6".into()]
    } else {
        vec!["3".into(), "6".into()]
    };
    out.get_mut("dhcp.fttr_tr069")
        .unwrap()
        .options
        .insert("dhcp_option".into(), dhcp_options);
    add_section(
        &mut out,
        "firewall.fttr_tr069",
        "zone",
        &[
            ("name", "fttr_tr069"),
            ("network", "fttr_tr069"),
            ("input", "REJECT"),
            ("output", "ACCEPT"),
            ("forward", "REJECT"),
            ("masq", "0"),
            ("masq6", "0"),
        ],
    );
    add_section(
        &mut out,
        "firewall.fttr_tr069_dhcp",
        "rule",
        &[
            ("name", "FTTR management DHCP"),
            ("src", "fttr_tr069"),
            ("family", "ipv4"),
            ("proto", "udp"),
            ("dest_port", "67"),
            ("target", "ACCEPT"),
            ("enabled", if d.enabled && d.dhcp { "1" } else { "0" }),
        ],
    );
    add_section(
        &mut out,
        "firewall.fttr_tr069_acs",
        "rule",
        &[
            ("name", "FTTR management ACS"),
            ("src", "fttr_tr069"),
            ("family", "ipv4"),
            ("proto", "tcp"),
            ("dest_port", &d.port.to_string()),
            ("target", "ACCEPT"),
            (
                "enabled",
                if d.enabled && d.acs_enabled { "1" } else { "0" },
            ),
        ],
    );
    add_section(
        &mut out,
        "firewall.fttr_tr069_icmp",
        "rule",
        &[
            ("name", "FTTR management ICMP"),
            ("src", "fttr_tr069"),
            ("family", "ipv4"),
            ("proto", "icmp"),
            ("icmp_type", "echo-request"),
            ("target", "ACCEPT"),
            ("enabled", if d.enabled { "1" } else { "0" }),
        ],
    );
    out
}
fn add_section(out: &mut Sections, id: &str, kind: &str, fields: &[(&str, &str)]) {
    out.insert(
        id.into(),
        Section {
            kind: kind.into(),
            options: fields
                .iter()
                .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
                .collect(),
        },
    );
}
fn owned_vlan(id: &str, s: &Section) -> bool {
    let reserved = id == "network.fttr_tr069_vlan"
        || id
            .strip_prefix("network.fttr_tr069_vlan_")
            .is_some_and(|suffix| suffix == s.get("vid"));
    reserved
        && s.kind == "device"
        && s.get("type") == "8021q"
        && s.get("ifname") == "br-lan"
        && s.get("vid")
            .parse::<u32>()
            .is_ok_and(|v| (1..=4094).contains(&v) && s.get("name") == format!("br-lan.{v}"))
        && s.options.keys().all(|k| {
            matches!(
                k.as_str(),
                "type" | "ifname" | "name" | "vid" | "egress_qos_mapping"
            )
        })
}
fn resources(d: &Desired, previous: Option<&Desired>) -> Sections {
    let mut out = expected(d);
    if let Some(previous) = previous {
        for (id, s) in expected(previous)
            .into_iter()
            .filter(|(id, _)| id.starts_with("network.fttr_tr069_vlan_"))
        {
            out.entry(id).or_insert(s);
        }
        let ports = out
            .get_mut("network.fttr_tr069_bridge")
            .unwrap()
            .options
            .get_mut("ports")
            .unwrap();
        for vlan in &previous.management.vlans {
            let name = format!("br-lan.{vlan}");
            if !ports.contains(&name) {
                ports.push(name);
            }
        }
        if previous.address != d.address || previous.prefix != d.prefix {
            for (id, mut s) in expected(previous)
                .into_iter()
                .filter(|(id, _)| id == "network.fttr_tr069" || id.starts_with("dhcp."))
            {
                let id = id.replace("fttr_tr069", "fttr_tr069_previous");
                if s.kind == "dhcp" {
                    s.options
                        .insert("interface".into(), vec!["fttr_tr069_previous".into()]);
                }
                out.insert(id, s);
            }
            out.get_mut("firewall.fttr_tr069").unwrap().options.insert(
                "network".into(),
                vec!["fttr_tr069".into(), "fttr_tr069_previous".into()],
            );
        }
        if previous.port != d.port && previous.acs_enabled {
            add_section(
                &mut out,
                "firewall.fttr_tr069_previous_acs",
                "rule",
                &[
                    ("name", "FTTR previous management ACS"),
                    ("src", "fttr_tr069"),
                    ("family", "ipv4"),
                    ("proto", "tcp"),
                    ("dest_port", &previous.port.to_string()),
                    ("target", "ACCEPT"),
                    ("enabled", "1"),
                ],
            );
        }
    }
    out
}
#[derive(Clone, Serialize)]
struct Action {
    operation: String,
    key: String,
    value: Vec<String>,
}
fn plan(d: &Desired, actual: &Sections) -> Result<Vec<Action>, String> {
    plan_resources(d, None, actual)
}
fn plan_resources(
    d: &Desired,
    previous: Option<&Desired>,
    actual: &Sections,
) -> Result<Vec<Action>, String> {
    let desired = resources(d, previous);
    let mut actions = Vec::new();
    let members = &desired["network.fttr_tr069_bridge"].options["ports"];
    for (id, s) in actual {
        if (id == "network.fttr_tr069_vlan" || id.starts_with("network.fttr_tr069_vlan_"))
            && !owned_vlan(id, s)
        {
            return Err(format!(
                "reserved management VLAN resource was changed externally: {id}"
            ));
        }
        if s.kind == "zone" && id != "firewall.fttr_tr069" && s.get("name") == "fttr_tr069" {
            return Err(format!("duplicate management firewall zone: {id}"));
        }
        if !desired.contains_key(id)
            && s.kind == "device"
            && (s.get("name") == "br-tr069" || members.iter().any(|name| s.get("name") == name))
            && !owned_vlan(id, s)
        {
            return Err(format!(
                "VLAN/bridge device belongs to another section: {id}"
            ));
        }
        if !desired.contains_key(id) && owned_vlan(id, s) {
            if actual.iter().any(|(other, section)| {
                !desired.contains_key(other)
                    && (section.get("device") == s.get("name")
                        || section
                            .options
                            .get("ports")
                            .is_some_and(|ports| ports.iter().any(|p| p == s.get("name"))))
            }) {
                return Err(format!(
                    "obsolete management VLAN is used by another network: {id}"
                ));
            }
            actions.push(Action {
                operation: "delete".into(),
                key: id.clone(),
                value: vec![],
            });
        }
        if s.kind == "interface"
            && !matches!(
                id.as_str(),
                "network.fttr_tr069" | "network.fttr_tr069_previous"
            )
            && (s.get("device") == "br-tr069" || members.iter().any(|p| s.get("device") == p))
        {
            return Err(format!(
                "management device is assigned to another interface: {id}"
            ));
        }
        if s.kind == "device"
            && id != "network.fttr_tr069_bridge"
            && s.options
                .get("ports")
                .is_some_and(|ports| ports.iter().any(|p| members.contains(p) || p == "br-tr069"))
        {
            return Err(format!("management VLAN is a port of another bridge: {id}"));
        }
        if s.kind == "interface" && id != "network.fttr_tr069" {
            for address in s.options.get("ipaddr").into_iter().flatten() {
                let address = if address.contains('/') {
                    address.clone()
                } else {
                    let prefix = s
                        .get("netmask")
                        .parse::<Ipv4Addr>()
                        .map(|m| u32::from(m).count_ones())
                        .unwrap_or(24);
                    format!("{address}/{prefix}")
                };
                if let Ok((ip, prefix)) = cidr(&address) {
                    let common = prefix.min(d.prefix);
                    let mask = u32::MAX << (32 - common);
                    if (u32::from(ip) & mask)
                        == (u32::from(d.address.parse::<Ipv4Addr>().unwrap()) & mask)
                    {
                        return Err(format!("management subnet overlaps {id}"));
                    }
                }
            }
        }
        if s.kind == "forwarding" && (s.get("src") == "fttr_tr069" || s.get("dest") == "fttr_tr069")
        {
            return Err(format!("cross-zone management forwarding exists: {id}"));
        }
        if s.kind == "rule"
            && !desired.contains_key(id)
            && s.get("target") == "ACCEPT"
            && matches!(s.get("src"), "*" | "fttr_tr069")
            && s.get("enabled") != "0"
        {
            return Err(format!(
                "external firewall rule bypasses management isolation: {id}"
            ));
        }
        if s.kind == "zone" && id != "firewall.fttr_tr069" {
            if let Some(networks) = s.options.get("network") {
                if networks
                    .iter()
                    .any(|n| n.split_whitespace().any(|n| n == "fttr_tr069"))
                {
                    actions.push(Action {
                        operation: "replace_list".into(),
                        key: format!("{id}.network"),
                        value: networks
                            .iter()
                            .flat_map(|n| n.split_whitespace())
                            .filter(|n| *n != "fttr_tr069")
                            .map(str::to_string)
                            .collect(),
                    });
                }
            }
        }
    }
    for (id, s) in desired {
        if let Some(old) = actual.get(&id) {
            if old.kind != s.kind {
                return Err(format!("resource type conflict: {id}"));
            }
            if s.kind == "device"
                && (old.get("type") != s.get("type")
                    || old.get("name") != s.get("name")
                    || (s.get("type") == "8021q" && old.get("ifname") != s.get("ifname")))
            {
                return Err(format!(
                    "same-named device is used by another network: {id}"
                ));
            }
            if (s.kind == "zone"
                && (!old.get("name").is_empty() && old.get("name") != "fttr_tr069"
                    || old.options.contains_key("device")
                    || old.options.contains_key("subnet")))
                || (s.kind == "interface" && !matches!(old.get("proto"), "" | "static"))
                || (s.kind == "dhcp"
                    && !old.get("interface").is_empty()
                    && old.get("interface") != s.get("interface"))
            {
                return Err(format!(
                    "same-named resource is used by another network: {id}"
                ));
            }
        } else {
            actions.push(Action {
                operation: "set".into(),
                key: id.clone(),
                value: vec![s.kind.clone()],
            });
        }
        for (key, value) in s.options {
            let old = actual.get(&id).and_then(|s| s.options.get(&key));
            if old != Some(&value) {
                actions.push(Action {
                    operation: if value.len() > 1
                        || matches!(key.as_str(), "egress_qos_mapping" | "ports")
                    {
                        "replace_list"
                    } else {
                        "set"
                    }
                    .into(),
                    key: format!("{id}.{key}"),
                    value,
                });
            }
        }
        // Remove stale fields that would override the managed IPv4/IPv6 policy.
        for key in ["netmask", "ip6addr", "ip6assign"] {
            if actual.get(&id).is_some_and(|s| s.options.contains_key(key)) {
                actions.push(Action {
                    operation: "delete".into(),
                    key: format!("{id}.{key}"),
                    value: vec![],
                });
            }
        }
    }
    Ok(actions)
}
fn apply(actions: &[Action]) -> Result<(), String> {
    let staging_dir =
        std::env::temp_dir().join(format!("fttr-mgmt-staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging_dir);
    std::fs::create_dir_all(&staging_dir).map_err(|e| format!("cannot create staging dir: {e}"))?;

    let mut uci = match openwrt_uci::Uci::new() {
        Ok(u) => u,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(format!("cannot initialize UCI context for apply: {e}"));
        }
    };
    if let Err(e) = uci.set_savedir(&staging_dir) {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(format!("cannot set isolated staging savedir: {e}"));
    }

    let mut touched_packages = std::collections::BTreeSet::new();

    for a in actions {
        let parts: Vec<&str> = a.key.splitn(3, '.').collect();
        match a.operation.as_str() {
            "set" => {
                if parts.len() == 2 {
                    if let Err(e) = uci.set(parts[0], parts[1], "", &a.value[0]) {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        return Err(format!("uci set {} failed: {e}", a.key));
                    }
                } else if parts.len() == 3 {
                    if let Err(e) = uci.set(parts[0], parts[1], parts[2], &a.value[0]) {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        return Err(format!("uci set {} failed: {e}", a.key));
                    }
                } else {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(format!("invalid UCI key format: {}", a.key));
                }
                touched_packages.insert(parts[0].to_string());
            }
            "replace_list" => {
                if parts.len() == 3 {
                    if let Err(e) = uci.replace_list(parts[0], parts[1], parts[2], &a.value) {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        return Err(format!("uci replace_list {} failed: {e}", a.key));
                    }
                } else {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(format!("invalid UCI list key format: {}", a.key));
                }
                touched_packages.insert(parts[0].to_string());
            }
            "delete" => {
                if parts.len() == 2 {
                    if let Err(e) = uci.delete(parts[0], parts[1], None) {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        return Err(format!("uci delete {} failed: {e}", a.key));
                    }
                } else if parts.len() == 3 {
                    if let Err(e) = uci.delete(parts[0], parts[1], Some(parts[2])) {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        return Err(format!("uci delete {} failed: {e}", a.key));
                    }
                } else {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(format!("invalid UCI key format: {}", a.key));
                }
                touched_packages.insert(parts[0].to_string());
            }
            "reload_runtime" => (),
            _ => {
                let _ = std::fs::remove_dir_all(&staging_dir);
                return Err("invalid management plan action".into());
            }
        }
    }

    // Verify touched packages have no external uncommitted modifications in /tmp/.uci
    for package in &touched_packages {
        let ext_delta = std::path::Path::new("/tmp/.uci").join(package);
        if ext_delta.exists()
            && std::fs::metadata(&ext_delta)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(format!(
                "package {package} has external pending uncommitted UCI changes; aborting apply"
            ));
        }
    }

    for package in &touched_packages {
        if let Err(e) = uci.commit(package) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(format!("uci commit {package} failed: {e}"));
        }
    }
    let _ = std::fs::remove_dir_all(&staging_dir);
    // Validate and install isolation before activating the new interface.
    run("fw4", &["check"])?;
    run("/etc/init.d/firewall", &["reload"])?;
    let mut ubus = openwrt_ubus::Ubus::connect(None)
        .map_err(|e| format!("cannot connect to ubus for network reload: {e}"))?;
    crate::platform::openwrt::NetifdClient::new(&mut ubus).reload()?;
    run("/etc/init.d/dnsmasq", &["reload"])?;
    Ok(())
}
fn runtime_plan(d: &Desired, previous: Option<&Desired>, actions: &mut Vec<Action>) {
    #[cfg(feature = "acs")]
    if !crate::acs::configuration_matches(
        d.enabled && d.acs_enabled,
        &format!("{}:{}", d.address, d.port),
        &d.path,
        &d.username,
        &d.password,
    ) {
        actions.push(Action {
            operation: "configure_listener".into(),
            key: "acs.endpoint".into(),
            value: vec![d.management.acs_url.clone()],
        });
    }
    if !d.enabled {
        return;
    }
    let ready = openwrt_ubus::Ubus::connect(None)
        .ok()
        .and_then(|mut ubus| {
            crate::platform::openwrt::NetifdClient::new(&mut ubus)
                .status("fttr_tr069")
                .ok()
        })
        .is_some_and(|s| {
            s.up && s
                .ipv4_address
                .iter()
                .any(|a| a.address == d.address && a.mask == d.prefix)
        });
    let firewall = run("nft", &["-j", "list", "table", "inet", "fw4"])
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|v| verify_firewall(d, previous, &v).is_ok());
    if !ready
        || !firewall
        || verify_bridge(d, previous).is_err()
        || verify_dhcp(d, "fttr_tr069").is_err()
    {
        actions.push(Action {
            operation: "reload_runtime".into(),
            key: "management.runtime".into(),
            value: vec!["network, firewall or DHCP runtime requires reconciliation".into()],
        });
    }
}
fn configure_listener(d: &Desired, previous: Option<&Desired>) -> Result<(), String> {
    #[cfg(feature = "acs")]
    {
        if !crate::acs::configuration_matches(
            d.enabled && d.acs_enabled,
            &format!("{}:{}", d.address, d.port),
            &d.path,
            &d.username,
            &d.password,
        ) {
            if let Some(p) = previous {
                crate::acs::reconfigure(
                    p.enabled && p.acs_enabled,
                    &format!("{}:{}", p.address, p.port),
                    &p.path,
                    &p.username,
                    &p.password,
                    false,
                )?;
            }
        }
        crate::acs::reconfigure(
            d.enabled && d.acs_enabled,
            &format!("{}:{}", d.address, d.port),
            &d.path,
            &d.username,
            &d.password,
            previous.is_some(),
        )
    }
    #[cfg(not(feature = "acs"))]
    {
        let _ = previous;
        if d.enabled && d.acs_enabled {
            Err("ACS feature is not compiled".into())
        } else {
            Ok(())
        }
    }
}
fn execute(d: Desired, apply_authorized: bool) {
    let previous = coordinator().lock().unwrap().journal.previous.clone();
    let result = (|| -> Result<bool, String> {
        let actual = discover()?;
        phase(Phase::Comparing);
        let mut actions = if !d.enabled {
            if let Some(p) = &previous {
                cleanup_actions(&d, p, &actual)?
            } else {
                plan(&d, &actual)?
            }
        } else {
            plan_resources(&d, previous.as_ref(), &actual)?
        };
        runtime_plan(&d, previous.as_ref(), &mut actions);
        {
            let mut c = coordinator().lock().unwrap();
            c.state = Phase::Planning;
            c.plan = serde_json::to_value(&actions).unwrap_or_default();
        }
        if !apply_authorized {
            return Ok(false);
        }
        phase(Phase::Applying);
        let actions: Vec<_> = actions
            .into_iter()
            .filter(|a| a.operation != "configure_listener")
            .collect();
        if !actions.is_empty() {
            let mut c = coordinator().lock().unwrap();
            c.set_ready(false);
            c.journal.pending = true;
            persist(&mut c)?;
            drop(c);
            apply(&actions)?;
        }
        let retained = if d.enabled { previous.as_ref() } else { None };
        phase(Phase::Verifying);
        verify_network(&d, retained)?;
        phase(Phase::ConfiguringListener);
        configure_listener(&d, retained)?;
        Ok(true)
    })();
    let mut c = coordinator().lock().unwrap();
    c.busy = false;
    match result {
        Ok(true) => {
            if !d.enabled {
                c.journal.previous = None;
                c.journal.cleanup_pending = false;
            }
            c.journal.applied = Some(d);
            c.journal.applied_epoch = c.journal.epoch;
            c.journal.pending = false;
            match persist(&mut c) {
                Ok(()) => {
                    c.set_ready(true);
                    c.state = if c.journal.previous.is_some() {
                        Phase::WaitingDevices
                    } else {
                        Phase::Ready
                    };
                    c.error = None;
                    c.next_retry = 0;
                    c.attempts = 0;
                }
                Err(e) => {
                    c.set_ready(false);
                    c.state = Phase::Blocked;
                    c.error = Some(e);
                }
            }
        }
        Ok(false) => {
            c.state = if c.plan.as_array().is_some_and(|a| a.is_empty())
                && c.journal
                    .applied
                    .as_ref()
                    .is_some_and(|a| a.version() == d.version())
            {
                Phase::Verified
            } else {
                Phase::AwaitingApply
            };
        }
        Err(e) => {
            if apply_authorized {
                c.set_ready(false);
            }
            c.attempts = c.attempts.saturating_add(1);
            c.next_retry = now() + 5u64.saturating_mul(1u64 << c.attempts.min(6)).min(300);
            c.state = if apply_authorized {
                Phase::RetryWait
            } else {
                Phase::Blocked
            };
            c.error = Some(e);
        }
    }
}
fn cleanup_actions(
    d: &Desired,
    previous: &Desired,
    actual: &Sections,
) -> Result<Vec<Action>, String> {
    let old = resources(d, Some(previous));
    let desired = expected(d);
    let mut remaining = actual.clone();
    let mut deletions = Vec::new();
    for (id, s) in old.iter().filter(|(id, _)| !desired.contains_key(*id)) {
        if let Some(found) = remaining.remove(id) {
            if found.kind != s.kind
                || s.options
                    .iter()
                    .any(|(k, v)| found.options.get(k) != Some(v))
            {
                return Err(format!(
                    "previous management resource changed outside the authorized plan: {id}"
                ));
            }
            deletions.push(Action {
                operation: "delete".into(),
                key: id.clone(),
                value: vec![],
            });
        }
    }
    let mut actions = plan(d, &remaining)?;
    actions.extend(deletions);
    Ok(actions)
}
fn cleanup_previous(d: Desired) {
    let previous = coordinator().lock().unwrap().journal.previous.clone();
    let result = (|| -> Result<(), String> {
        if let Some(previous) = &previous {
            let actions = cleanup_actions(&d, previous, &discover()?)?;
            if !actions.is_empty() {
                apply(&actions)?;
            }
        }
        verify_network(&d, None)?;
        configure_listener(&d, None)?;
        #[cfg(feature = "acs")]
        crate::acs::retire_previous();
        Ok(())
    })();
    let mut c = coordinator().lock().unwrap();
    c.busy = false;
    match result {
        Ok(()) => {
            c.journal.previous = None;
            c.journal.cleanup_pending = false;
            if let Err(e) = persist(&mut c) {
                c.set_ready(false);
                c.state = Phase::Blocked;
                c.error = Some(e);
            } else {
                c.set_ready(true);
                c.state = Phase::Ready;
                c.error = None;
                c.next_retry = 0;
            }
        }
        Err(e) => {
            c.state = Phase::RetryWait;
            c.error = Some(e);
            c.next_retry = now() + 15;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_skips_unchanged_writes_and_retries_failed_commits() {
        let mut journal = Journal::default();
        let mut saved = None;
        let mut writes = 0;
        let mut commit = |_: &[u8]| {
            writes += 1;
            Ok(())
        };
        persist_journal(&journal, &mut saved, &mut commit).unwrap();
        for _ in 0..100 {
            persist_journal(&journal, &mut saved, &mut commit).unwrap();
        }
        assert_eq!(writes, 1);
        journal.pending = true;
        // Even an error after rename must not mark the new snapshot durable.
        assert!(persist_journal(&journal, &mut saved, |_| {
            Err(std::io::Error::other("directory sync failed"))
        })
        .is_err());
        assert!(saved.is_none());
        let mut retries = 0;
        persist_journal(&journal, &mut saved, |_| {
            retries += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(retries, 1);
        persist_journal(&journal, &mut saved, |_| panic!("unchanged commit")).unwrap();
        journal.pending = false;
        persist_journal(&journal, &mut saved, |_| {
            retries += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(retries, 2);
        // A rollback to the previously committed journal must also write after
        // a failed attempt, because rename may already have replaced the file.
        journal.pending = true;
        assert!(persist_journal(&journal, &mut saved, |_| {
            Err(std::io::Error::other("directory sync failed"))
        })
        .is_err());
        journal.pending = false;
        persist_journal(&journal, &mut saved, |_| {
            retries += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(retries, 3);
    }
    #[test]
    fn failed_worker_spawn_is_reported_and_allows_a_later_launch() {
        let mut c = Coordinator {
            journal: Journal {
                pending: true,
                ..Default::default()
            },
            saved: None,
            requires_apply: false,
            busy: false,
            state: Phase::Discovering,
            error: None,
            plan: json!([]),
            next_retry: 0,
            attempts: 0,
            ready: true,
            last_discovery: 0,
        };
        let error = finish_spawn::<()>(
            &mut c,
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        )
        .unwrap_err();
        assert!(!c.busy);
        assert!(matches!(c.state, Phase::RetryWait));
        assert_eq!(c.error.as_deref(), Some(error.as_str()));
        assert!(c.next_retry >= now());
        assert!(c.ready);
        assert!(c.journal.pending);
        let (tx, rx) = std::sync::mpsc::channel();
        finish_spawn(
            &mut c,
            std::thread::Builder::new().spawn(move || {
                tx.send(()).unwrap();
            }),
        )
        .unwrap();
        assert!(c.busy);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn vlan_membership_changes_share_resources_and_remove_only_owned_devices() {
        let old = desired();
        let mut next = old.clone();
        next.management.vlans = vec![46, 47];
        assert!(!endpoint_changed(&old, &next));
        let expanded = expected(&next);
        assert_eq!(
            expanded["network.fttr_tr069_bridge"].options["ports"],
            ["br-lan.46", "br-lan.47"]
        );
        assert_eq!(
            expanded.values().filter(|s| s.kind == "interface").count(),
            1
        );
        assert_eq!(expanded.values().filter(|s| s.kind == "dhcp").count(), 1);
        assert!(plan(&next, &expanded).unwrap().is_empty());
        let reduced = plan(&old, &expanded).unwrap();
        assert!(reduced
            .iter()
            .any(|a| a.operation == "delete" && a.key == "network.fttr_tr069_vlan_47"));
        assert!(!reduced.iter().any(|a| a.key == "network.fttr_tr069.ipaddr"));
        let mut legacy = expected(&old);
        let device = legacy.remove("network.fttr_tr069_vlan_46").unwrap();
        legacy.insert("network.fttr_tr069_vlan".into(), device);
        assert!(plan(&old, &legacy)
            .unwrap()
            .iter()
            .any(|a| a.key == "network.fttr_tr069_vlan" && a.operation == "delete"));
        let mut foreign = expanded.clone();
        foreign
            .get_mut("network.fttr_tr069_vlan_47")
            .unwrap()
            .options
            .insert("mtu".into(), vec!["1400".into()]);
        assert!(plan(&old, &foreign).is_err());
        let mut foreign = expanded;
        add_section(
            &mut foreign,
            "network.other",
            "interface",
            &[("device", "br-lan.47")],
        );
        assert!(plan(&old, &foreign)
            .err()
            .unwrap()
            .contains("used by another network"));
    }
    #[test]
    fn old_journal_is_rejected_instead_of_authorizing_a_new_topology() {
        let journal = Journal {
            applied: Some(desired()),
            ..Default::default()
        };
        let mut value = serde_json::to_value(journal).unwrap();
        value["applied"]["management"]
            .as_object_mut()
            .unwrap()
            .remove("vlans");
        assert!(serde_json::from_value::<Journal>(value).is_err());
    }
    fn desired() -> Desired {
        let mut s = Section::default();
        for (k, v) in [("vlan", "46"), ("address", "192.0.2.1/24")] {
            s.options.insert(k.into(), vec![v.into()]);
        }
        Desired::parse(&s, None).unwrap()
    }
    #[test]
    fn manual_is_default_and_url_has_one_address_source() {
        let d = desired();
        assert!(!d.automatic);
        assert_eq!(d.management.acs_url, "http://192.0.2.1:7547/cwmp");
        assert!(cidr("192.0.2.0/24").is_err());
        assert!(cidr("192.0.2.255/24").is_err());
    }
    #[test]
    fn acs_credentials_must_be_configured_as_a_pair() {
        let mut s = Section::default();
        for (k, v) in [("vlan", "46"), ("address", "192.0.2.1/24")] {
            s.options.insert(k.into(), vec![v.into()]);
        }
        let mut a = Section::default();
        a.options.insert("username".into(), vec!["cpe-user".into()]);
        let error = Desired::parse(&s, Some(&a)).err().unwrap();
        assert!(error.contains("both be set"));
        a.options.insert("password".into(), vec!["secret".into()]);
        let d = Desired::parse(&s, Some(&a)).unwrap();
        assert_eq!(d.username, "cpe-user");
        assert_eq!(d.password, "secret");
    }
    #[test]
    fn blank_system_plan_converges_without_duplicate_resources() {
        let d = desired();
        let p = plan(&d, &Sections::new()).unwrap();
        assert!(p
            .iter()
            .any(|a| a.key == "firewall.fttr_tr069.input" && a.value == ["REJECT"]));
        assert!(plan(&d, &expected(&d)).unwrap().is_empty());
        assert!(!p.iter().any(|a| a.key.contains("forwarding")));
    }
    #[test]
    fn isolation_removes_duplicate_zone_membership_and_blocks_forwarding() {
        let d = desired();
        let mut actual = expected(&d);
        add_section(
            &mut actual,
            "firewall.lan",
            "zone",
            &[("network", "lan fttr_tr069")],
        );
        let p = plan(&d, &actual).unwrap();
        assert!(p
            .iter()
            .any(|a| a.key == "firewall.lan.network" && a.value == ["lan"]));
        add_section(
            &mut actual,
            "firewall.escape",
            "forwarding",
            &[("src", "fttr_tr069"), ("dest", "lan")],
        );
        assert!(plan(&d, &actual).is_err());
    }
    #[test]
    fn discovery_preserves_anonymous_zone_indices_and_detects_subnet_conflicts() {
        let d = desired();
        let a=parse_export("config zone\n list network 'lan'\nconfig zone 'other'\n option name 'other'\nconfig zone\n option name 'third'\n","firewall").unwrap();
        assert!(a.contains_key("firewall.@zone[2]"));
        let mut a = Sections::new();
        add_section(
            &mut a,
            "network.lan",
            "interface",
            &[("ipaddr", "192.0.2.10/24")],
        );
        assert!(plan(&d, &a).is_err());
    }
    #[test]
    fn saved_manual_configuration_never_inherits_an_old_authorization() {
        let old = desired();
        let mut next = old.clone();
        next.address = "198.51.100.1".into();
        next.management.acs_url = "http://198.51.100.1:7547/cwmp".into();
        let mut journal = Journal {
            authorized: Some(old.clone()),
            applied: Some(old),
            epoch: 1,
            ..Default::default()
        };
        assert!(!application_authorized(&journal, &next));
        journal.pending = true;
        assert!(!application_authorized(&journal, &next));
        journal.authorized = Some(next.clone());
        let restored: Journal =
            serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(application_authorized(&restored, &next));
        journal.pending = false;
        assert!(!application_authorized(&journal, &next));
        next.automatic = true;
        assert!(application_authorized(&journal, &next));
    }
    #[test]
    fn migration_retains_owned_old_path_and_cleanup_is_idempotent() {
        let old = desired();
        let mut next = old.clone();
        next.address = "198.51.100.1".into();
        next.management.vlan = 47;
        next.management.vlans = vec![47];
        next.port = 7548;
        next.management.acs_url = "http://198.51.100.1:7548/cwmp".into();
        assert!(!subnets_overlap(&old, &next));
        let actions = plan_resources(&next, Some(&old), &expected(&old)).unwrap();
        assert!(actions
            .iter()
            .any(|a| a.key == "network.fttr_tr069_previous"));
        let staged = resources(&next, Some(&old));
        assert!(plan_resources(&next, Some(&old), &staged)
            .unwrap()
            .is_empty());
        assert_eq!(
            staged["firewall.fttr_tr069"].options["network"],
            ["fttr_tr069", "fttr_tr069_previous"]
        );
        let cleanup = cleanup_actions(&next, &old, &staged).unwrap();
        assert!(cleanup
            .iter()
            .any(|a| a.operation == "delete" && a.key == "network.fttr_tr069_previous"));
        assert!(cleanup_actions(&next, &old, &expected(&next))
            .unwrap()
            .is_empty());
        let mut modified = staged;
        modified
            .get_mut("network.fttr_tr069_previous")
            .unwrap()
            .options
            .insert("ipaddr".into(), vec!["203.0.113.1/24".into()]);
        assert!(cleanup_actions(&next, &old, &modified).is_err());
    }
    #[test]
    fn runtime_dhcp_validation_checks_pool_tag_range_mask_and_lease() {
        let d = desired();
        assert!(dhcp_range_matches(
            &d,
            "fttr_tr069",
            "dhcp-range=set:fttr_tr069,192.0.2.100,192.0.2.199,255.255.255.0,12h\n"
        ));
        for text in [
            "dhcp-range=set:lan,192.0.2.100,192.0.2.199,255.255.255.0,12h",
            "dhcp-range=set:fttr_tr069,192.0.2.100,192.0.2.200,255.255.255.0,12h",
            "dhcp-range=set:fttr_tr069,192.0.2.100,192.0.2.199,255.255.0.0,12h",
            "dhcp-range=set:fttr_tr069,192.0.2.100,192.0.2.199,255.255.255.0,1h",
        ] {
            assert!(!dhcp_range_matches(&d, "fttr_tr069", text));
        }
    }
    #[test]
    fn firewall_validation_requires_exact_ports_isolated_forwarding_and_dispatch() {
        let d = desired();
        let exception = |name: &str, proto: &str, port: u16| json!({"rule":{"chain":"input_fttr_tr069","comment":name,"expr":[{"match":{"left":{"meta":{"key":"nfproto"}},"right":"ipv4"}},{"match":{"left":{"payload":{"protocol":proto,"field":"dport"}},"right":port}},{"accept":null}]}});
        let icmp_rule = json!({"rule":{"chain":"input_fttr_tr069","comment":"FTTR management ICMP","expr":[{"match":{"left":{"payload":{"protocol":"icmp","field":"type"}},"right":"echo-request"}},{"accept":null}]}});
        let mut rules = json!({"nftables":[exception("FTTR management ACS","tcp",7547),exception("FTTR management DHCP","udp",67),icmp_rule,{"rule":{"chain":"input_fttr_tr069","expr":[{"reject":null}]}},{"rule":{"chain":"forward_fttr_tr069","expr":[{"reject":null}]}},{"rule":{"chain":"input","expr":[{"match":{"left":{"meta":{"key":"iifname"}},"right":"br-tr069"}},{"jump":{"target":"input_fttr_tr069"}}]}},{"rule":{"chain":"forward","expr":[{"match":{"left":{"meta":{"key":"iifname"}},"right":"br-tr069"}},{"jump":{"target":"forward_fttr_tr069"}}]}}]});
        assert!(verify_firewall(&d, None, &rules).is_ok());
        rules["nftables"][0]["rule"]["expr"][1]["match"]["right"] = json!(22);
        assert!(verify_firewall(&d, None, &rules).is_err());
        rules["nftables"][0] = exception("FTTR management ACS", "tcp", 7547);
        rules["nftables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"rule":{"chain":"forward_fttr_tr069","expr":[{"accept":null}]}}));
        assert!(verify_firewall(&d, None, &rules).is_err());
    }
    #[test]
    fn management_gateway_advertises_router_option_while_preserving_isolated_forward_reject() {
        let mut d = desired();
        assert!(!d.management_gateway);
        let default_expected = expected(&d);
        assert_eq!(
            default_expected["dhcp.fttr_tr069"].options["dhcp_option"],
            vec!["3", "6"]
        );
        assert_eq!(
            default_expected["firewall.fttr_tr069"].options["forward"],
            vec!["REJECT"]
        );

        d.management_gateway = true;
        let gateway_expected = expected(&d);
        assert_eq!(
            gateway_expected["dhcp.fttr_tr069"].options["dhcp_option"],
            vec!["3,192.0.2.1", "6"]
        );
        assert_eq!(
            gateway_expected["firewall.fttr_tr069"].options["forward"],
            vec!["REJECT"]
        );
    }
}
