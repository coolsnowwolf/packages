//! Platform-aware Airoha NPU/PPE admission and fw4 hook reconciliation.
use super::command;
use fttr_api::NpuAdmissionModel;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MASK: &str = "/sys/module/h3c_fmcs/parameters/bridge_offload_mask";
pub const DIRECTIONS: &str = "/sys/module/airoha_eth/parameters/fttr_offload_directions";
pub const NET: &str = "/sys/class/net";
pub const POLL_FAST: Duration = Duration::from_secs(2);
pub const POLL_STEADY: Duration = Duration::from_secs(10);
const FULL_CHECK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub bridge: String,
    pub routed: bool,
    pub onu_bridge: bool,
    pub direction_mask: u8,
    pub mask: u16,
    // Includes ifindex, MTU, MAC and learning: even same-mask changes must drain.
    pub ports: Vec<(String, Vec<String>)>,
}

impl Topology {
    pub fn target_token(&self, model: NpuAdmissionModel) -> u16 {
        match model {
            NpuAdmissionModel::PortMask => self.mask,
            NpuAdmissionModel::DirectionOnly => u16::from(self.direction_mask),
        }
    }
}

pub(crate) fn text(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path).map(|value| value.trim().to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub bridge: String,
    pub enabled: bool,
    pub wan_upstream: bool,
    pub wan_downstream: bool,
    pub onu_bridge: bool,
    pub firewall_offload: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            bridge: "br-lan".into(),
            enabled: true,
            wan_upstream: false,
            wan_downstream: false,
            onu_bridge: true,
            firewall_offload: false,
        }
    }
}

impl Policy {
    /// Missing WAN options stay experimental/disabled; invalid flags stay disabled.
    pub fn directions(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        u8::from(self.wan_upstream)
            | (u8::from(self.wan_downstream) << 1)
            | (u8::from(self.onu_bridge) << 2)
    }

    fn load() -> io::Result<Self> {
        let fttr = crate::config::load().map_err(io::Error::other)?;
        let mut uci = openwrt_uci::Uci::new().map_err(io::Error::other)?;
        let mut get = |package, section, key| {
            uci.get(package, section, key)
                .map_err(|e| io::Error::other(format!("{package}.{section}.{key} read error: {e}")))
        };
        Ok(Self {
            bridge: fttr
                .option("offload", "bridge")
                .unwrap_or_else(|| "br-lan".into()),
            enabled: fttr.option("offload", "enabled").as_deref().unwrap_or("1") == "1",
            wan_upstream: fttr.option("offload", "wan_upstream").as_deref() == Some("1"),
            wan_downstream: fttr.option("offload", "wan_downstream").as_deref() == Some("1"),
            onu_bridge: fttr
                .option("offload", "onu_bridge")
                .as_deref()
                .unwrap_or("1")
                == "1",
            firewall_offload: {
                let software = get("firewall", "@defaults[0]", "flow_offloading")?;
                let hardware = get("firewall", "@defaults[0]", "flow_offloading_hw")?;
                software.as_deref() == Some("1") && hardware.as_deref() == Some("1")
            },
        })
    }
}

pub fn topology(policy: &Policy, net: &Path, mask: &Path) -> io::Result<Topology> {
    topology_for_model(NpuAdmissionModel::PortMask, policy, net, mask)
}

pub fn topology_for_model(
    model: NpuAdmissionModel,
    policy: &Policy,
    net: &Path,
    gate_path: &Path,
) -> io::Result<Topology> {
    let bridge = policy.bridge.as_str();
    let requested_directions = policy.directions();
    let mut topology = Topology {
        bridge: bridge.into(),
        routed: requested_directions & 3 != 0,
        onu_bridge: requested_directions & 4 != 0,
        direction_mask: 0,
        mask: 0,
        ports: Vec::new(),
    };
    let valid_bridge = !bridge.is_empty()
        && bridge.len() <= 15
        && bridge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte));
    if !valid_bridge || requested_directions == 0 || !policy.firewall_offload {
        return Ok(topology);
    }
    let gate_ready = match model {
        NpuAdmissionModel::PortMask => {
            (!topology.routed || gate_path.with_file_name("routed_offload_mask").exists())
                && (!topology.onu_bridge || gate_path.exists())
        }
        NpuAdmissionModel::DirectionOnly => gate_path.exists(),
    };
    if !gate_ready
        || text(net.join("fttr0/carrier")).ok().as_deref() != Some("1")
        || text(net.join(bridge).join("bridge/vlan_filtering"))
            .ok()
            .as_deref()
            != Some("0")
    {
        return Ok(topology);
    }
    let max_port = match model {
        NpuAdmissionModel::PortMask => 16,
        NpuAdmissionModel::DirectionOnly => 128,
    };
    for port in 1..=max_port {
        let name = format!("fttr{port}");
        let path = net.join(&name);
        if text(path.join("carrier")).ok().as_deref() != Some("1")
            || text(path.join("brport/state")).ok().as_deref() != Some("3")
            || fs::read_link(path.join("master")).ok().as_deref()
                != Some(Path::new(&format!("../{bridge}")))
        {
            continue;
        }
        let identity = ["ifindex", "mtu", "address", "brport/learning"]
            .iter()
            .map(|field| text(path.join(field)))
            .collect::<io::Result<Vec<_>>>()?;
        if port <= 16 {
            topology.mask |= 1 << (port - 1);
        }
        topology.ports.push((name, identity));
    }
    let min_ports = if topology.routed { 1 } else { 2 };
    if topology.ports.len() < min_ports {
        topology.mask = 0;
        topology.direction_mask = 0;
    } else {
        let mut effective = 0u8;
        if topology.routed {
            effective |= requested_directions & 3;
        }
        if topology.onu_bridge && topology.ports.len() >= 2 {
            effective |= 4;
        }
        topology.direction_mask = effective;
    }
    Ok(topology)
}

#[derive(Default)]
pub struct Reconciler {
    pub last: Option<Topology>,
    pub stable: u8,
    pub armed: bool,
    pub reload_requested: bool,
    cache: Option<PolicyCache>,
}

struct PolicyCache {
    policy: Policy,
    files: Vec<Option<Vec<u8>>>,
    verified: Instant,
}

fn policy_files(config: &Path, delta: &Path) -> io::Result<Vec<Option<Vec<u8>>>> {
    [config, delta]
        .into_iter()
        .flat_map(|dir| ["fttr", "firewall", "network"].map(|name| dir.join(name)))
        .map(|path| match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        })
        .collect()
}

impl Reconciler {
    /// Two unchanged observations after withdrawal, also on kernel invalidation.
    pub fn observe(&mut self, topology: Topology, current: u16) -> bool {
        self.observe_for_model(NpuAdmissionModel::PortMask, topology, current)
    }

    pub fn observe_for_model(
        &mut self,
        model: NpuAdmissionModel,
        topology: Topology,
        current: u16,
    ) -> bool {
        let target = topology.target_token(model);
        if self.last.as_ref() != Some(&topology) || (self.armed && current != target) {
            self.last = Some(topology);
            self.stable = 0;
            self.armed = false;
            self.reload_requested = false;
            return false;
        }
        self.stable = self.stable.saturating_add(1);
        self.last.as_ref().unwrap().target_token(model) != 0 && self.stable >= 2
    }

    fn can_skip(
        &self,
        model: NpuAdmissionModel,
        topology: &Topology,
        current: u16,
        files: &[Option<Vec<u8>>],
        now: Instant,
    ) -> bool {
        let target = topology.target_token(model);
        self.armed
            && current != 0
            && current == target
            && self.last.as_ref() == Some(topology)
            && self.cache.as_ref().is_some_and(|cache| {
                cache.files == files && now.duration_since(cache.verified) < FULL_CHECK
            })
    }
}

pub fn text_offload_flag(table: &str) -> bool {
    named_offload_flag(table, "bridge", "fb")
}

fn named_offload_flag(table: &str, family: &str, name: &str) -> bool {
    let lines: Vec<_> = table
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.first().copied() == Some(format!("table {family} fw4 {{").as_str())
        && lines.get(1).copied() == Some(format!("flowtable {name} {{").as_str())
        && lines
            .iter()
            .any(|line| line.trim_end_matches(';') == "flags offload")
}

pub fn hooks_ready(table: &serde_json::Value, ports: &[(String, Vec<String>)], text: &str) -> bool {
    table["nftables"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            let flow = &entry["flowtable"];
            flow["family"] == "bridge"
                && flow["table"] == "fw4"
                && flow["name"] == "fb"
                && flow["hook"] == "ingress"
                && match flow.get("flags") {
                    Some(flags) => flags
                        .as_array()
                        .is_some_and(|flags| flags.iter().any(|flag| flag == "offload")),
                    None => text_offload_flag(text),
                }
                && flow["dev"].as_array().is_some_and(|devices| {
                    ports.iter().all(|(port, _)| {
                        devices
                            .iter()
                            .any(|device| device.as_str() == Some(port.as_str()))
                    })
                })
        })
    })
}

pub fn routed_hooks_ready(table: &serde_json::Value, text: &str) -> bool {
    table["nftables"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            let flow = &entry["flowtable"];
            flow["family"] == "inet"
                && flow["table"] == "fw4"
                && flow["name"] == "ft"
                && flow["hook"] == "ingress"
                && match flow.get("flags") {
                    Some(flags) => flags
                        .as_array()
                        .is_some_and(|flags| flags.iter().any(|flag| flag == "offload")),
                    None => named_offload_flag(text, "inet", "ft"),
                }
                && flow["dev"]
                    .as_array()
                    .is_some_and(|devices| devices.iter().any(|device| device == "fttr0"))
        })
    })
}

pub struct Admission {
    pub model: NpuAdmissionModel,
    pub path: PathBuf,
    pub directions_path: PathBuf,
    pub stopped: bool,
}

impl Admission {
    pub fn for_model(model: NpuAdmissionModel) -> Self {
        match model {
            NpuAdmissionModel::PortMask => Self::new(MASK),
            NpuAdmissionModel::DirectionOnly => Self::new_direction_only(DIRECTIONS),
        }
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::new_with_directions(path, DIRECTIONS)
    }

    pub fn new_with_directions(
        path: impl Into<PathBuf>,
        directions_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            model: NpuAdmissionModel::PortMask,
            path: path.into(),
            directions_path: directions_path.into(),
            stopped: false,
        }
    }

    pub fn new_direction_only(directions_path: impl Into<PathBuf>) -> Self {
        Self {
            model: NpuAdmissionModel::DirectionOnly,
            path: PathBuf::new(),
            directions_path: directions_path.into(),
            stopped: false,
        }
    }

    pub fn current_directions(&self) -> io::Result<u8> {
        match text(&self.directions_path) {
            Ok(value) => value.parse().map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    pub fn set_directions(&self, mask: u8) -> io::Result<()> {
        let mask = if self.stopped { 0 } else { mask };
        let path = self.directions_path.as_path();
        if !path.try_exists()? {
            if self.model == NpuAdmissionModel::PortMask {
                let routed = self.path.with_file_name("routed_offload_mask");
                if routed.try_exists()? && text(&routed)? != "0" {
                    fs::write(routed, "0\n")?;
                }
                if mask & 3 != 0 {
                    return Err(io::Error::other(
                        "WAN direction policy requires Airoha patch 933",
                    ));
                }
                return Ok(());
            }
            if mask != 0 {
                return Err(io::Error::other(
                    "fttr_offload_directions sysfs parameter unavailable",
                ));
            }
            return Ok(());
        }
        if text(path)? != mask.to_string() {
            fs::write(path, format!("{mask}\n"))?;
            if text(path)? != mask.to_string() {
                return Err(io::Error::other(
                    "NPU direction admission readback mismatch",
                ));
            }
            eprintln!("[*] NPU direction admission mask={mask}");
        }
        Ok(())
    }

    pub fn withdraw(&mut self) -> io::Result<()> {
        match self.model {
            NpuAdmissionModel::DirectionOnly => self.set_directions(0),
            NpuAdmissionModel::PortMask => {
                let directions = self.set_directions(0);
                let bridge = self.set(0);
                let routed = self.set_routed(0);
                directions.and(bridge).and(routed)
            }
        }
    }

    pub fn current(&self) -> io::Result<u16> {
        self.current_mode(false)
    }

    pub fn current_mode(&self, routed: bool) -> io::Result<u16> {
        let path = if routed {
            self.path.with_file_name("routed_offload_mask")
        } else {
            self.path.clone()
        };
        match text(path) {
            Ok(value) => value.parse().map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    pub fn set(&mut self, mask: u16) -> io::Result<()> {
        self.set_mode(false, mask)
    }

    pub fn set_routed(&mut self, mask: u16) -> io::Result<()> {
        self.set_mode(true, mask)
    }

    pub fn set_mode(&mut self, routed: bool, mask: u16) -> io::Result<()> {
        let mask = if self.stopped { 0 } else { mask };
        if self.current_mode(routed)? != mask {
            let path = if routed {
                self.path.with_file_name("routed_offload_mask")
            } else {
                self.path.clone()
            };
            fs::write(path, format!("{mask}\n"))?;
            eprintln!(
                "[*] NPU {} admission mask={mask}",
                if routed { "routed" } else { "bridge" }
            );
        }
        Ok(())
    }
}

fn current_token(adm: &Admission, routed: bool, onu_bridge: bool) -> io::Result<u16> {
    match adm.model {
        NpuAdmissionModel::DirectionOnly => Ok(u16::from(adm.current_directions()?)),
        NpuAdmissionModel::PortMask => {
            let r = if routed { adm.current_mode(true)? } else { 0 };
            let b = if onu_bridge {
                adm.current_mode(false)?
            } else {
                0
            };
            Ok(r | b)
        }
    }
}

async fn check_nft_flowtables(topology: &Topology) -> bool {
    let mut ready = true;
    if topology.routed {
        let json_table = command::run_async(
            "nft",
            &["-j", "list", "flowtable", "inet", "fw4", "ft"],
            Duration::from_secs(5),
        )
        .await;
        let routed_ready = match json_table {
            Ok(ref json_str) => match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(ref table) => {
                    let has_flags = table["nftables"].as_array().is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| entry["flowtable"].get("flags").is_some())
                    });
                    let text_listing = if has_flags {
                        String::new()
                    } else {
                        command::run_async(
                            "nft",
                            &["list", "flowtable", "inet", "fw4", "ft"],
                            Duration::from_secs(5),
                        )
                        .await
                        .unwrap_or_default()
                    };
                    routed_hooks_ready(table, &text_listing)
                }
                Err(_) => false,
            },
            Err(_) => false,
        };
        if !routed_ready {
            ready = false;
        }
    }

    if topology.onu_bridge && topology.ports.len() >= 2 {
        let json_table = command::run_async(
            "nft",
            &["-j", "list", "flowtable", "bridge", "fw4", "fb"],
            Duration::from_secs(5),
        )
        .await;
        let bridge_ready = match json_table {
            Ok(ref json_str) => match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(ref table) => {
                    let has_flags = table["nftables"].as_array().is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| entry["flowtable"].get("flags").is_some())
                    });
                    let text_listing = if has_flags {
                        String::new()
                    } else {
                        command::run_async(
                            "nft",
                            &["list", "flowtable", "bridge", "fw4", "fb"],
                            Duration::from_secs(5),
                        )
                        .await
                        .unwrap_or_default()
                    };
                    hooks_ready(table, &topology.ports, &text_listing)
                }
                Err(_) => false,
            },
            Err(_) => false,
        };
        if !bridge_ready {
            ready = false;
        }
    }
    ready
}

/// Reconcile NPU admission state for either `PortMask` (HM2004-DU) or `DirectionOnly` (AN4475-G2).
pub async fn reconcile(
    admission: &Mutex<Admission>,
    state: &mut Reconciler,
) -> io::Result<Duration> {
    let net = Path::new(NET);
    let (model, gate_path) = {
        let adm = admission.lock().unwrap();
        let gate = match adm.model {
            NpuAdmissionModel::PortMask => adm.path.clone(),
            NpuAdmissionModel::DirectionOnly => adm.directions_path.clone(),
        };
        (adm.model, gate)
    };
    let cached_routed = state
        .cache
        .as_ref()
        .is_some_and(|cache| cache.policy.directions() & 3 != 0);
    let cached_bridge = state
        .cache
        .as_ref()
        .is_some_and(|cache| cache.policy.directions() & 4 != 0);
    let current_mask = {
        let adm = admission.lock().unwrap();
        current_token(&adm, cached_routed, cached_bridge)?
    };
    let files = policy_files(Path::new("/etc/config"), Path::new("/tmp/.uci"))?;

    if let Some(cache) = &state.cache {
        if model == NpuAdmissionModel::PortMask {
            let policy_dirs = if cache.files == files {
                cache.policy.directions()
            } else {
                0
            };
            admission.lock().unwrap().set_directions(policy_dirs)?;
        }
        let live = topology_for_model(model, &cache.policy, net, &gate_path)?;
        if state.can_skip(model, &live, current_mask, &files, Instant::now()) {
            return Ok(POLL_STEADY);
        }
        if state.armed
            && (state.last.as_ref() != Some(&live)
                || current_mask != live.target_token(model)
                || cache.files != files)
        {
            admission.lock().unwrap().withdraw()?;
        }
    }

    let policy = Policy::load()?;
    if policy_files(Path::new("/etc/config"), Path::new("/tmp/.uci"))? != files {
        admission.lock().unwrap().withdraw()?;
        *state = Reconciler::default();
        return Ok(POLL_FAST);
    }
    let topology = topology_for_model(model, &policy, net, &gate_path)?;
    if model == NpuAdmissionModel::PortMask {
        let mut adm = admission.lock().unwrap();
        if !topology.routed {
            adm.set_routed(0)?;
        }
        if !topology.onu_bridge {
            adm.set(0)?;
        }
        adm.set_directions(policy.directions())?;
    }
    let verified = state
        .cache
        .as_ref()
        .map(|cache| cache.verified)
        .unwrap_or_else(Instant::now);
    state.cache = Some(PolicyCache {
        policy,
        files,
        verified,
    });
    let current = {
        let adm = admission.lock().unwrap();
        current_token(&adm, topology.routed, topology.onu_bridge)?
    };

    if !state.observe_for_model(model, topology.clone(), current) {
        let mut adm = admission.lock().unwrap();
        match model {
            NpuAdmissionModel::DirectionOnly => adm.withdraw()?,
            NpuAdmissionModel::PortMask => {
                if topology.routed {
                    adm.set_routed(0)?;
                }
                if topology.onu_bridge {
                    adm.set(0)?;
                }
            }
        }
        return Ok(POLL_FAST);
    }

    let topology = state.last.as_ref().unwrap();
    if !check_nft_flowtables(topology).await {
        {
            let mut adm = admission.lock().unwrap();
            match model {
                NpuAdmissionModel::DirectionOnly => adm.withdraw()?,
                NpuAdmissionModel::PortMask => {
                    if topology.routed {
                        adm.set_routed(0)?;
                    }
                    if topology.onu_bridge {
                        adm.set(0)?;
                    }
                }
            }
        }
        state.armed = false;
        if !state.reload_requested {
            state.reload_requested = true;
            eprintln!("[*] NPU refreshing fw4 hooks for live FTTR ports");
            if let Err(error) =
                command::run_async("/etc/init.d/firewall", &["reload"], Duration::from_secs(5))
                    .await
            {
                eprintln!("[-] NPU fw4 refresh failed: {error}");
            }
        }
        return Ok(POLL_FAST);
    }

    state.reload_requested = false;
    let cache = state.cache.as_ref().unwrap();
    let current_check = {
        let adm = admission.lock().unwrap();
        current_token(&adm, topology.routed, topology.onu_bridge)?
    };
    if policy_files(Path::new("/etc/config"), Path::new("/tmp/.uci"))? != cache.files
        || topology_for_model(model, &cache.policy, net, &gate_path)? != *topology
        || current_check != current
    {
        admission.lock().unwrap().withdraw()?;
        state.last = None;
        state.armed = false;
        state.stable = 0;
        return Ok(POLL_FAST);
    }
    {
        let mut adm = admission.lock().unwrap();
        match model {
            NpuAdmissionModel::DirectionOnly => {
                adm.set_directions(topology.direction_mask)?;
            }
            NpuAdmissionModel::PortMask => {
                if topology.routed {
                    adm.set_routed(topology.mask)?;
                }
                if topology.onu_bridge && topology.ports.len() >= 2 {
                    adm.set(topology.mask)?;
                }
            }
        }
    }
    state.armed = true;
    state.cache.as_mut().unwrap().verified = Instant::now();
    Ok(POLL_STEADY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_policy_defaults_disable_wan_and_coexistence_is_allowed() {
        let mut policy = Policy::default();
        assert_eq!(policy.directions(), 4);
        policy.wan_upstream = true;
        assert!(topology(&policy, Path::new("/unused"), Path::new("/unused")).is_ok());
        policy.onu_bridge = false;
        assert_eq!(policy.directions(), 1);
        policy.wan_downstream = true;
        assert_eq!(policy.directions(), 3);
        policy.enabled = false;
        assert_eq!(policy.directions(), 0);
    }

    fn pair(identity: &str) -> Topology {
        Topology {
            bridge: "br-lan".into(),
            routed: false,
            onu_bridge: true,
            direction_mask: 4,
            mask: 3,
            ports: vec![
                ("fttr1".into(), vec![identity.into()]),
                ("fttr2".into(), vec![]),
            ],
        }
    }

    #[test]
    fn invalidation_and_same_mask_identity_changes_require_settling() {
        let mut state = Reconciler::default();
        assert!(!state.observe(pair("old"), 0));
        assert!(!state.observe(pair("old"), 0));
        assert!(state.observe(pair("old"), 0));
        state.armed = true;
        assert!(!state.observe(pair("old"), 0));
        assert!(!state.observe(pair("old"), 0));
        assert!(state.observe(pair("old"), 0));
        assert!(!state.observe(pair("new"), 3));
        assert!(!state.observe(pair("new"), 0));
        assert!(state.observe(pair("new"), 0));
        let mut disabled = pair("new");
        disabled.mask = 0;
        assert!(!state.observe(disabled, 3));
        state.armed = true;
        state.last = Some(pair("old"));
        assert!(!state.observe(pair("old"), 7));
    }

    #[test]
    fn direction_only_reconciler_settles_and_detects_kernel_revocation() {
        let mut state = Reconciler::default();
        let topo = Topology {
            bridge: "br-lan".into(),
            routed: true,
            onu_bridge: true,
            direction_mask: 7,
            mask: 3,
            ports: vec![
                ("fttr1".into(), vec!["11".into()]),
                ("fttr2".into(), vec!["12".into()]),
            ],
        };
        assert!(!state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        assert!(!state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        assert!(state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        state.armed = true;
        assert!(!state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        assert!(!state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        assert!(state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo.clone(), 0));
        state.armed = true;
        assert!(!state.observe_for_model(NpuAdmissionModel::DirectionOnly, topo, 3));
    }

    #[test]
    fn steady_cache_requires_full_topology_policy_mask_and_periodic_hook_checks() {
        let now = Instant::now();
        let files = vec![Some(b"enabled".to_vec())];
        let state = Reconciler {
            last: Some(pair("old")),
            armed: true,
            cache: Some(PolicyCache {
                policy: Policy::default(),
                files: files.clone(),
                verified: now,
            }),
            ..Default::default()
        };
        assert!(state.can_skip(NpuAdmissionModel::PortMask, &pair("old"), 3, &files, now));
        for mask in [0, 1, 7] {
            assert!(!state.can_skip(NpuAdmissionModel::PortMask, &pair("old"), mask, &files, now));
        }
        assert!(!state.can_skip(NpuAdmissionModel::PortMask, &pair("new"), 3, &files, now));
        let mut added = pair("old");
        added.mask = 7;
        added.ports.push(("fttr3".into(), vec![]));
        assert!(!state.can_skip(NpuAdmissionModel::PortMask, &added, 3, &files, now));
        assert!(!state.can_skip(NpuAdmissionModel::PortMask, &pair("old"), 3, &[None], now));
        assert!(!state.can_skip(
            NpuAdmissionModel::PortMask,
            &pair("old"),
            3,
            &files,
            now + FULL_CHECK
        ));
    }

    #[test]
    fn policy_watch_includes_uncommitted_changes_and_removal() {
        let root = std::env::temp_dir().join(format!("fttr-npu-policy-{}", std::process::id()));
        let config = root.join("config");
        let delta = root.join("delta");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&delta).unwrap();
        let initial = policy_files(&config, &delta).unwrap();
        fs::write(config.join("fttr"), "enabled").unwrap();
        let committed = policy_files(&config, &delta).unwrap();
        assert_ne!(initial, committed);
        fs::write(delta.join("firewall"), "disabled").unwrap();
        assert_ne!(committed, policy_files(&config, &delta).unwrap());
        fs::remove_file(delta.join("firewall")).unwrap();
        assert_eq!(committed, policy_files(&config, &delta).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hook_validation_requires_exact_port_names_and_hardware_flag() {
        let mut table = serde_json::json!({"nftables": [{"flowtable": {
            "family": "bridge", "table": "fw4", "name": "fb", "hook": "ingress",
            "flags": ["offload"], "dev": ["fttr10", "fttr2"]
        }}]});
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["dev"][0] = "fttr1".into();
        assert!(hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["hook"] = "egress".into();
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        table["nftables"][0]["flowtable"]["hook"] = "ingress".into();
        table["nftables"][0]["flowtable"]["flags"] = serde_json::json!([]);
        let text = "table bridge fw4 {\n flowtable fb {\n flags offload\n counter\n }\n}";
        assert!(!hooks_ready(&table, &pair("").ports, text));
        table["nftables"][0]["flowtable"]
            .as_object_mut()
            .unwrap()
            .remove("flags");
        assert!(hooks_ready(&table, &pair("").ports, text));
        assert!(!hooks_ready(&table, &pair("").ports, ""));
        assert!(!hooks_ready(
            &table,
            &pair("").ports,
            &text.replace("fw4", "other")
        ));
        assert!(!hooks_ready(
            &table,
            &pair("").ports,
            &text.replace("flags offload", "counter")
        ));
    }

    #[test]
    fn routed_hooks_require_the_transport_and_the_named_inet_hardware_table() {
        let mut table = serde_json::json!({"nftables": [{"flowtable": {
            "family": "inet", "table": "fw4", "name": "ft", "hook": "ingress",
            "flags": ["offload"], "dev": ["fttr0", "lan1"]
        }}]});
        assert!(routed_hooks_ready(&table, ""));
        table["nftables"][0]["flowtable"]["dev"] = serde_json::json!(["fttr1", "lan1"]);
        assert!(!routed_hooks_ready(&table, ""));
        table["nftables"][0]["flowtable"]["dev"] = serde_json::json!(["fttr0", "lan1"]);
        let text = "table inet fw4 {\n flowtable ft {\n flags offload\n counter\n }\n}";
        table["nftables"][0]["flowtable"]["flags"] = serde_json::json!([]);
        assert!(!routed_hooks_ready(&table, text));
        table["nftables"][0]["flowtable"]
            .as_object_mut()
            .unwrap()
            .remove("flags");
        assert!(routed_hooks_ready(&table, text));
        assert!(!routed_hooks_ready(&table, &text.replace("inet", "bridge")));
    }

    #[test]
    fn discovery_respects_policy_topology_and_direction_only_thresholds() {
        let root = std::env::temp_dir().join(format!("fttr-npu-topology-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mask = root.join("mask");
        let directions = root.join("fttr_offload_directions");
        for (name, value) in [
            ("fttr0/carrier", "1"),
            ("br-lan/bridge/vlan_filtering", "0"),
            ("fttr1/carrier", "1"),
            ("fttr2/carrier", "1"),
            ("fttr1/brport/state", "3"),
            ("fttr2/brport/state", "3"),
            ("fttr1/brport/learning", "1"),
            ("fttr2/brport/learning", "1"),
            ("fttr1/ifindex", "11"),
            ("fttr2/ifindex", "12"),
            ("fttr1/mtu", "1500"),
            ("fttr2/mtu", "1500"),
            ("fttr1/address", "00:00:00:00:00:01"),
            ("fttr2/address", "00:00:00:00:00:02"),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        for port in ["fttr1", "fttr2"] {
            #[cfg(unix)]
            std::os::unix::fs::symlink("../br-lan", root.join(port).join("master")).unwrap();
            #[cfg(not(unix))]
            let _ = fs::write(root.join(port).join("master"), "../br-lan");
        }
        fs::write(&mask, "0").unwrap();
        fs::write(&directions, "0").unwrap();
        let policy = Policy {
            firewall_offload: true,
            ..Default::default()
        };
        let discover = |policy: &Policy| topology(policy, &root, &mask).unwrap().mask;
        assert_eq!(discover(&policy), 3);

        // DirectionOnly: 2 ports with all directions requested => direction_mask 7.
        let all_policy = Policy {
            firewall_offload: true,
            wan_upstream: true,
            wan_downstream: true,
            onu_bridge: true,
            ..Default::default()
        };
        let d2 = topology_for_model(
            NpuAdmissionModel::DirectionOnly,
            &all_policy,
            &root,
            &directions,
        )
        .unwrap();
        assert_eq!(d2.direction_mask, 7);

        // A high-numbered live port participates in direction-only admission,
        // but never in the HM2004 16-bit hardware mask.
        for (field, value) in [
            ("carrier", "1"),
            ("brport/state", "3"),
            ("brport/learning", "1"),
            ("ifindex", "128"),
            ("mtu", "1500"),
            ("address", "00:00:00:00:00:80"),
        ] {
            let path = root.join("fttr128").join(field);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("../br-lan", root.join("fttr128/master")).unwrap();
        #[cfg(unix)]
        {
            let high = topology_for_model(
                NpuAdmissionModel::DirectionOnly,
                &all_policy,
                &root,
                &directions,
            )
            .unwrap();
            assert!(high.ports.iter().any(|(name, _)| name == "fttr128"));
            assert_eq!(high.mask, 3);
            assert_eq!(topology(&policy, &root, &mask).unwrap().ports.len(), 2);
            fs::write(root.join("fttr128/brport/state"), "2").unwrap();
        }

        // With only 1 port forwarding, routed bits (3) remain admissible while onu_bridge bit (4) is withheld.
        fs::write(root.join("fttr2/brport/state"), "2").unwrap();
        let d1 = topology_for_model(
            NpuAdmissionModel::DirectionOnly,
            &all_policy,
            &root,
            &directions,
        )
        .unwrap();
        assert_eq!(d1.direction_mask, 3);
        let bridge_only = topology_for_model(
            NpuAdmissionModel::DirectionOnly,
            &policy,
            &root,
            &directions,
        )
        .unwrap();
        assert_eq!(bridge_only.direction_mask, 0);
        fs::write(root.join("fttr2/brport/state"), "3").unwrap();

        let before = topology(&policy, &root, &mask).unwrap();
        fs::write(root.join("fttr1/mtu"), "1400").unwrap();
        let after = topology(&policy, &root, &mask).unwrap();
        assert_eq!(before.mask, after.mask);
        assert_ne!(before, after);
        fs::write(root.join("fttr1/mtu"), "1500").unwrap();
        assert_eq!(
            discover(&Policy {
                enabled: false,
                ..policy.clone()
            }),
            0
        );
        assert_eq!(
            discover(&Policy {
                bridge: "../br-lan".into(),
                ..policy.clone()
            }),
            0
        );
        assert_eq!(discover(&Policy::default()), 0);
        fs::write(root.join("br-lan/bridge/vlan_filtering"), "1").unwrap();
        assert_eq!(discover(&policy), 0);
        fs::write(root.join("br-lan/bridge/vlan_filtering"), "0").unwrap();
        fs::write(root.join("fttr0/carrier"), "0").unwrap();
        assert_eq!(discover(&policy), 0);
        let _ = fs::remove_dir_all(root);
    }
}
