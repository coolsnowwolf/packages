//! Controller-driven, read-only capability discovery. RPC transport is injected.
use super::transport::{Command, Reply};
use super::{
    discovery::{Evidence, Resolution, ScalarType},
    identity::Identity,
    registry,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
const DISCOVERY_ROOTS: [&str; 3] = [
    "InternetGatewayDevice.DeviceInfo.",
    "InternetGatewayDevice.WANDevice.",
    "InternetGatewayDevice.LANDevice.",
];

#[derive(Clone)]
pub struct Discovery {
    requested: String,
    stage: &'static str,
    task: Option<u64>,
    names: BTreeMap<String, bool>,
    names_scope: usize,
    values: BTreeMap<String, (ScalarType, String)>,
    pending: VecDeque<Vec<String>>,
    awaited: Vec<String>,
    identity: Option<Identity>,
    epoch: u64,
    selected: Option<String>,
    reason: String,
    schema: String,
    cache_hit: bool,
    retries: usize,
    retry_at: u64,
}
impl Discovery {
    pub(crate) fn progress(&self) -> (&'static str, Option<u64>, usize, u64) {
        (self.stage, self.task, self.pending.len(), self.retry_at)
    }
    pub(crate) fn deadline(&self) -> u64 {
        self.retry_at
    }
    pub fn new(requested: &str) -> Self {
        Self {
            requested: requested.into(),
            stage: "pending",
            task: None,
            names: BTreeMap::new(),
            names_scope: 0,
            values: BTreeMap::new(),
            pending: VecDeque::new(),
            awaited: Vec::new(),
            identity: None,
            epoch: 0,
            selected: None,
            reason: String::new(),
            schema: String::new(),
            cache_hit: false,
            retries: 0,
            retry_at: 0,
        }
    }
    pub fn ready(&self) -> bool {
        self.stage == "resolved"
    }
    pub fn blocked(&self) -> bool {
        self.stage == "blocked"
    }
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub fn observation_changed(&self, identity: &Identity, epoch: u64) -> bool {
        self.ready()
            && (self.identity.as_ref().is_some_and(|old| old != identity) || self.epoch != epoch)
    }
    pub fn wan_fields(&self) -> BTreeMap<String, Vec<String>> {
        let mut result = BTreeMap::new();
        for path in self.names.keys() {
            for kind in ["WANIPConnection", "WANPPPConnection"] {
                if let Some((parent, tail)) = path.split_once(&format!(".{kind}.")) {
                    if let Some((instance, _)) = tail.split_once('.') {
                        let connection = format!("{parent}.{kind}.{instance}.");
                        let root = format!("{parent}.");
                        result.entry(connection.clone()).or_insert_with(|| {
                            let fields: BTreeSet<_> = registry::available()
                                .into_iter()
                                .flat_map(|c| c.wan.classification_paths(&root, &connection))
                                .filter(|name| self.names.contains_key(name))
                                .collect();
                            fields.into_iter().collect()
                        });
                    }
                }
            }
        }
        result
    }
    pub fn protected_wan(&self) -> BTreeSet<String> {
        let mut protected = BTreeSet::new();
        for contract in registry::available() {
            for (path, (_, service)) in &self.values {
                let Some(connection) = path.strip_suffix(contract.wan.service.name) else {
                    continue;
                };
                if !connection.ends_with('.') || !connection.contains(".WANConnectionDevice.") {
                    continue;
                }
                let Some((parent, _)) = connection
                    .rsplit_once(".WANIPConnection.")
                    .or_else(|| connection.rsplit_once(".WANPPPConnection."))
                else {
                    continue;
                };
                let root = format!("{parent}.");
                let management =
                    super::codec::service_tokens(service).any(|v| v.eq_ignore_ascii_case("TR069"));
                let internet = super::codec::service_tokens(service)
                    .any(|v| v.eq_ignore_ascii_case("INTERNET"));
                let tagged = contract.wan.tagged(|name| {
                    let field = if name == contract.wan.vlan.name {
                        contract.wan.vlan
                    } else {
                        contract.wan.enabled?
                    };
                    self.values
                        .get(&field.path(&root, connection))
                        .map(|(_, v)| v.clone())
                });
                if management || internet && tagged != Some(true) {
                    protected.insert(connection.into());
                }
            }
        }
        protected
    }
    pub fn endpoints(
        &self,
        device: &crate::provisioning::model::Device,
    ) -> Result<super::path::EndpointMap, String> {
        let contract = registry::lookup(self.selected().ok_or("contract unresolved")?)?;
        let mut map = super::path::EndpointMap::default();
        let mut add = |endpoint: &crate::provisioning::model::EndpointRef| -> Result<(), String> {
            use crate::provisioning::model::EndpointRef;
            let (kind, instance) = match endpoint {
                EndpointRef::Ssid { radio, slot }
                    if matches!(radio, 2 | 5) && (1..=4).contains(slot) =>
                {
                    let slots = contract
                        .wlan
                        .radio_slots
                        .ok_or("radio instance mapping is unverified")?;
                    (
                        "WLANConfiguration",
                        slots[if *radio == 2 { 0 } else { 1 }][(*slot - 1) as usize],
                    )
                }
                EndpointRef::Lan(n) => ("LANEthernetInterfaceConfig", *n),
                _ => return Err("logical endpoint mapping is unverified".into()),
            };
            let path = format!("InternetGatewayDevice.LANDevice.1.{kind}.{instance}");
            if kind == "LANEthernetInterfaceConfig"
                && !self
                    .names
                    .keys()
                    .any(|n| n.starts_with(&format!("{path}.")))
            {
                return Err(format!("LAN endpoint absent from inventory: {endpoint:?}"));
            }
            map.0.insert(endpoint.clone(), path);
            Ok(())
        };
        for item in &device.items {
            if let Some(wlan) = &item.wlan_intent {
                add(&wlan.endpoint)?;
            }
            if contract.capability_groups.binding == super::contract::Support::Supported {
                if let Some(binding) = &item.binding_intent {
                    for endpoint in &binding.endpoints {
                        add(endpoint)?;
                    }
                }
            }
        }
        Ok(map)
    }
    pub fn invalidate(&mut self, reason: &str) {
        if let Some(identity) = &self.identity {
            super::cache::invalidate_record(&identity.optical_serial);
        }
        self.stage = "blocked";
        self.selected = None;
        self.task = None;
        self.reason = reason.into();
    }
    pub fn view(&self) -> Value {
        let contract = self
            .selected
            .as_deref()
            .and_then(|id| registry::lookup(id).ok());
        json!({"state":self.stage,"requested":self.requested,"contract":self.selected,"task_id":self.task,"reason":self.reason,"schema":self.schema,"contract_version":registry::VERSION,"cache_hit":self.cache_hit,"retry_attempt":self.retries,"retry_at":self.retry_at,"capabilities":contract.map(|c| c.capabilities())})
    }
    fn retry(&mut self, reason: &str) {
        const DELAYS: [u64; 3] = [5, 15, 30];
        let Some(delay) = DELAYS.get(self.retries) else {
            self.invalidate(&format!("retry_exhausted: {reason}"));
            return;
        };
        self.retry_at = discovery_now().saturating_add(*delay);
        self.retries += 1;
        self.stage = "retrying";
        self.reason = reason.into();
        self.task = None;
        self.names.clear();
        self.names_scope = 0;
        self.values.clear();
        self.pending.clear();
        self.awaited.clear();
    }
    pub fn tick(
        &mut self,
        device: &str,
        identity: Identity,
        epoch: u64,
        reachable: bool,
        mut rpc: impl FnMut(Command) -> Reply,
    ) {
        if !reachable {
            return;
        }
        if self.identity.as_ref().is_some_and(|old| old != &identity)
            || self.identity.is_some() && self.epoch != epoch
        {
            if self.ready() {
                self.invalidate("identity, firmware or BOOTSTRAP epoch changed during the operation; start a new plan");
                return;
            }
            *self = Self::new(&self.requested);
        }
        self.identity = Some(identity);
        self.epoch = epoch;
        if !super::identity::associated(self.identity.as_ref().unwrap()) {
            self.invalidate("CWMP identity does not match the optical device");
            return;
        }
        if self.ready() || self.blocked() {
            return;
        }
        if self.stage == "retrying" {
            if discovery_now() < self.retry_at {
                return;
            }
            self.stage = "pending";
        }
        if let Some(id) = self.task {
            let result = rpc(Command::Task { id });
            match result["state"].as_str() {
                Some("queued" | "sent") => return,
                Some("completed") => (),
                Some("unknown" | "expired") | None => {
                    self.retry("read-only discovery transport/result unavailable");
                    return;
                }
                _ => {
                    self.invalidate("read-only capability discovery failed");
                    return;
                }
            }
            self.task = None;
            let Some(rows) = result.result().rows() else {
                if result["result_status"] == "resource_exhausted" {
                    self.retry("missing discovery response or resource_exhausted");
                } else {
                    self.invalidate("malformed discovery response: missing parameter evidence");
                }
                return;
            };
            if rows.len() > 32768 {
                self.invalidate("parameter tree exceeds discovery bound");
                return;
            }
            if self.stage == "values" && rows.len() != self.awaited.len() {
                self.invalidate("incomplete value response");
                return;
            }
            for row in rows {
                let Some(name) = row["name"].as_str() else {
                    self.invalidate("invalid parameter name");
                    return;
                };
                if self.stage == "names" {
                    if !name.starts_with(DISCOVERY_ROOTS[self.names_scope]) {
                        self.invalidate("parameter name outside requested discovery scope");
                        return;
                    }
                    let writable = matches!(row["writable"].as_str(), Some("1" | "true"))
                        || row["writable"] == true;
                    if self
                        .names
                        .insert(name.into(), writable)
                        .is_some_and(|old| old != writable)
                    {
                        self.invalidate("conflicting writable evidence");
                        return;
                    }
                } else {
                    if !self.awaited.iter().any(|n| n == name) {
                        self.invalidate("unexpected value response path");
                        return;
                    }
                    let kind = match row["type"]
                        .as_str()
                        .unwrap_or("")
                        .rsplit(':')
                        .next()
                        .unwrap_or("")
                    {
                        "string" => ScalarType::String,
                        "boolean" => ScalarType::Boolean,
                        "unsignedInt" => ScalarType::UnsignedInt,
                        _ => {
                            self.invalidate(
                                "unsupported or missing CWMP type in capability evidence",
                            );
                            return;
                        }
                    };
                    let Some(value) = row["value"].as_str() else {
                        self.invalidate("missing capability value");
                        return;
                    };
                    if self
                        .values
                        .insert(name.into(), (kind, value.into()))
                        .is_some()
                    {
                        self.invalidate("duplicate capability value");
                        return;
                    }
                }
            }
            if self.stage == "names" {
                if self.names_scope + 1 < DISCOVERY_ROOTS.len() {
                    self.names_scope += 1;
                    self.stage = "pending";
                } else {
                    let selected: Vec<_> = self
                        .names
                        .keys()
                        .filter(|name| self.is_signature(name))
                        .cloned()
                        .collect();
                    self.pending = selected.chunks(32).map(|c| c.to_vec()).collect();
                    self.stage = "values";
                }
            }
        }
        if self.stage == "pending" {
            let reply = rpc(Command::queue(
                device,
                "GetParameterNames",
                json!({"path":DISCOVERY_ROOTS[self.names_scope],"next_level":false}),
            ));
            self.task = reply["id"].as_u64();
            self.stage = "names";
            if self.task.is_none() {
                if super::transport::backpressure(&reply) {
                    self.stage = "pending";
                    self.reason = "waiting for task ingress capacity".into();
                } else {
                    self.invalidate("cannot queue parameter tree discovery");
                }
            }
        } else if let Some(names) = self.pending.pop_front() {
            self.awaited = names.clone();
            let reply = rpc(Command::queue(
                device,
                "GetParameterValues",
                json!({"names":names}),
            ));
            self.task = reply["id"].as_u64();
            if self.task.is_none() {
                if super::transport::backpressure(&reply) {
                    self.pending.push_front(std::mem::take(&mut self.awaited));
                    self.reason = "waiting for task ingress capacity".into();
                } else {
                    self.invalidate("cannot queue capability value discovery");
                }
            }
        } else {
            self.resolve();
        }
    }
    fn is_signature(&self, name: &str) -> bool {
        if matches!(
            name,
            "InternetGatewayDevice.DeviceInfo.SoftwareVersion"
                | "InternetGatewayDevice.DeviceInfo.HardwareVersion"
        ) {
            return true;
        }
        if name.contains(".WLANConfiguration.") && name.ends_with(".BeaconType") {
            return true;
        }
        registry::available().iter().any(|c| {
            [
                Some(c.wan.service),
                Some(c.wan.vlan),
                Some(c.wan.priority),
                c.wan.enabled,
                c.wan.cloud_cpe,
                c.wan.binding,
            ]
            .into_iter()
            .flatten()
            .chain(c.binding_fields())
            .any(|f| name.ends_with(&format!(".{}", f.name)))
        })
    }
    fn resolve(&mut self) {
        if self
            .names
            .keys()
            .filter(|n| self.is_signature(n))
            .any(|n| !self.values.contains_key(n))
        {
            self.invalidate("incomplete capability values");
            return;
        }
        let evidence: Vec<_> = self
            .values
            .iter()
            .map(|(path, (kind, _))| Evidence {
                path: path.clone(),
                writable: self.names.get(path).copied(),
                kind: Some(*kind),
            })
            .collect();
        let mut identity = self.identity.clone().unwrap();
        identity.software_version = self
            .values
            .get("InternetGatewayDevice.DeviceInfo.SoftwareVersion")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        identity.hardware_version = self
            .values
            .get("InternetGatewayDevice.DeviceInfo.HardwareVersion")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let known = if self.requested != "auto" {
            registry::lookup(&self.requested).ok()
        } else {
            super::identity::specialization(&identity)
        };
        let mut candidates = BTreeSet::new();
        let mut objects = BTreeSet::new();
        for path in self.names.keys() {
            for kind in ["WANIPConnection", "WANPPPConnection"] {
                if let Some((parent, rest)) = path.split_once(&format!(".{kind}.")) {
                    if let Some((instance, _)) = rest.split_once('.') {
                        objects.insert((parent.to_owned(), kind.to_owned(), instance.to_owned()));
                    }
                }
            }
        }
        for (parent, kind, instance) in objects {
            let root = format!("{parent}.");
            let connection = format!("{parent}.{kind}.{instance}.");
            match super::discovery::match_wan(&evidence, &root, &connection) {
                Resolution::Matched(c) => {
                    candidates.insert(c.vendor);
                }
                Resolution::Ambiguous(_) => {
                    self.invalidate("conflicting WAN vendor signatures");
                    return;
                }
                Resolution::Incomplete => {
                    self.invalidate("incomplete WAN capability evidence; identity alone cannot authorize lifecycle operations");
                    return;
                }
            }
        }
        if candidates.len() > 1 {
            self.invalidate("different WAN instances expose conflicting contracts");
            return;
        }
        let selected = known.or_else(|| {
            if identity.optical_vendor == "HWTC" && candidates.contains("hw") {
                registry::lookup("hwtc-hw").ok()
            } else {
                registry::available()
                    .into_iter()
                    .find(|c| candidates.contains(c.vendor) && !c.writes_verified)
            }
        });
        let Some(contract) = selected else {
            self.invalidate("no coherent capability contract matched");
            return;
        };
        if candidates.is_empty() {
            self.invalidate("no WAN capability evidence; known or forced profiles cannot authorize lifecycle operations");
            return;
        }
        if !candidates.is_empty() && !candidates.contains(contract.vendor) {
            self.invalidate("identity contract conflicts with discovered parameters");
            return;
        }
        if matches!(
            contract.references,
            Some(super::contract::ReferenceEncoding::EndpointFlags { .. })
        ) {
            let mut connections = BTreeSet::new();
            for path in self.names.keys() {
                if let Some((parent, tail)) = path.split_once(".WANIPConnection.") {
                    if let Some((instance, _)) = tail.split_once('.') {
                        connections.insert((
                            format!("{parent}."),
                            format!("{parent}.WANIPConnection.{instance}."),
                        ));
                    }
                }
            }
            if connections.is_empty()
                || connections.iter().any(|(parent, connection)| {
                    contract.binding_fields().iter().any(|field| {
                        let path = field.path(parent, connection);
                        self.names.get(&path) != Some(&true)
                            || !self.values.get(&path).is_some_and(|(kind, value)| {
                                *kind == ScalarType::UnsignedInt
                                    && matches!(value.as_str(), "0" | "1")
                            })
                    })
                })
            {
                self.invalidate(
                    "binding switches require complete writable unsignedInt 0/1 evidence",
                );
                return;
            }
        }
        if !contract.writes_verified {
            self.selected = Some(contract.id.into());
            self.stage = "blocked";
            self.reason = "matched candidate has no verified write implementation".into();
            return;
        }
        if identity.software_version.is_empty() {
            self.invalidate("firmware identity is missing");
            return;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for (name, writable) in &self.names {
            name.hash(&mut hash);
            writable.hash(&mut hash);
            self.values
                .get(name)
                .map(|(k, _)| format!("{k:?}"))
                .hash(&mut hash);
        }
        self.schema = format!("{:016x}", hash.finish());
        let scope = json!({"identity":identity,"epoch":self.epoch,"schema":self.schema,"version":registry::VERSION,"requested":self.requested});
        self.cache_hit = super::cache::load_record(&identity.optical_serial)
            .is_some_and(|v| v["scope"] == scope && v["contract"] == contract.id);
        if let Err(reason) = super::cache::save_record(
            &identity.optical_serial,
            &json!({"scope":scope,"contract":contract.id}),
        ) {
            self.invalidate(&reason);
            return;
        }
        self.selected = Some(contract.id.into());
        self.stage = "resolved";
    }
}

fn discovery_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingress_backpressure_does_not_consume_discovery_retry_budget() {
        let mut probe = Discovery::new("auto");
        for _ in 0..20 {
            probe.retry_at = 0;
            probe.tick("test", identity(90), 1, true, |request| {
                assert_eq!(request.wire()["method"], "GetParameterNames");
                json!({"error":"queue full", "code":"backpressure"}).into()
            });
            assert_eq!(probe.stage, "pending");
            assert!(!probe.blocked());
            assert_eq!(probe.retries, 0);
        }
    }
    #[cfg(feature = "device-hwtc")]
    #[test]
    fn v152_requires_complete_current_binding_switch_evidence() {
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.";
        let connection = format!("{root}WANIPConnection.1.");
        let contract = registry::lookup("hwtc-v152-hw").unwrap();
        for bad in ["none", "missing", "readonly", "type", "value"] {
            let mut probe = Discovery::new("auto");
            let mut id = identity(20);
            id.product_class = "V152".into();
            probe.identity = Some(id);
            let firmware = "InternetGatewayDevice.DeviceInfo.SoftwareVersion";
            probe.names.insert(firmware.into(), false);
            probe
                .values
                .insert(firmware.into(), (ScalarType::String, "v1".into()));
            for (field, value) in [
                (contract.wan.service, "TR069"),
                (contract.wan.vlan, "46"),
                (contract.wan.priority, "0"),
            ] {
                let path = field.path(root, &connection);
                probe.names.insert(path.clone(), true);
                probe.values.insert(path, (field.kind, value.into()));
            }
            for (index, field) in contract.binding_fields().into_iter().enumerate() {
                if bad == "missing" && index == 11 {
                    continue;
                }
                let path = field.path(root, &connection);
                probe
                    .names
                    .insert(path.clone(), !(bad == "readonly" && index == 11));
                probe.values.insert(
                    path,
                    (
                        if bad == "type" && index == 11 {
                            ScalarType::String
                        } else {
                            ScalarType::UnsignedInt
                        },
                        if bad == "value" && index == 11 {
                            "2"
                        } else {
                            "0"
                        }
                        .into(),
                    ),
                );
            }
            probe.resolve();
            assert_eq!(probe.ready(), bad == "none", "{bad}: {}", probe.reason);
            if probe.ready() {
                assert_eq!(probe.selected(), Some("hwtc-v152-hw"));
            }
        }
        super::super::cache::invalidate_record("HWTC00000014");
    }
    fn identity(n: u32) -> Identity {
        Identity {
            optical_vendor: "HWTC".into(),
            optical_serial: format!("HWTC{n:08X}"),
            cwmp_serial: format!("48575443{n:08X}"),
            product_class: "K155-11".into(),
            software_version: String::new(),
            hardware_version: String::new(),
        }
    }
    fn run(discovery: &mut Discovery, identity: Identity, bad_type: bool) -> Vec<Value> {
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.WANIPConnection.1.";
        let fields = [
            (
                format!("{root}X_HW_SERVICELIST"),
                "xsd:string",
                "TR069_INTERNET",
            ),
            (format!("{root}X_HW_VLAN"), "xsd:unsignedInt", "0"),
            (format!("{root}X_HW_PRI"), "xsd:unsignedInt", "0"),
            (
                "InternetGatewayDevice.DeviceInfo.SoftwareVersion".into(),
                "xsd:string",
                "v1",
            ),
        ];
        let names: Vec<_> = fields
            .iter()
            .map(|(name, _, _)| json!({"name":name,"writable":"1"}))
            .collect();
        let values: Vec<_>=fields.iter().enumerate().map(|(i,(name,kind,value))|json!({"name":name,"type":if bad_type && i==1 {""} else {kind},"value":value})).collect();
        let mut replies = VecDeque::new();
        for (index, root) in DISCOVERY_ROOTS.iter().enumerate() {
            replies.push_back(json!({"id":index+1}));
            let scoped: Vec<_> = names
                .iter()
                .filter(|row| row["name"].as_str().unwrap().starts_with(root))
                .collect();
            replies.push_back(json!({"state":"completed","result":{"parameters":scoped}}));
        }
        replies.push_back(json!({"id":4}));
        replies.push_back(json!({"state":"completed","result":{"parameters":values}}));
        let mut requests = Vec::new();
        for _ in 0..5 {
            discovery.tick("test-child", identity.clone(), 7, true, |v| {
                requests.push(v.wire());
                replies
                    .pop_front()
                    .expect("unexpected discovery RPC")
                    .into()
            });
        }
        assert!(replies.is_empty());
        assert!(requests
            .iter()
            .filter_map(|r| r["method"].as_str())
            .all(|m| matches!(m, "GetParameterNames" | "GetParameterValues")));
        requests
    }
    #[test]
    #[cfg_attr(not(feature = "device-hwtc"), ignore = "requires HWTC contract")]
    fn readonly_discovery_resolves_profile_caches_schema_and_protects_management() {
        let identity = identity(1);
        let mut first = Discovery::new("auto");
        run(&mut first, identity.clone(), false);
        assert!(first.ready());
        assert_eq!(first.selected(), Some("hwtc-k155-hw"));
        assert!(first.protected_wan().contains(
            "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.WANIPConnection.1."
        ));
        let mut second = Discovery::new("auto");
        run(&mut second, identity.clone(), false);
        assert!(second.ready());
        assert!(second.cache_hit);
        second.tick("test-child", identity, 8, true, |_| {
            panic!(
                "epoch invalidation must not queue writes or replay discovery during an operation"
            )
        });
        assert!(second.blocked());
        assert!(super::super::cache::load_record("HWTC00000001").is_none());
    }
    #[test]
    fn missing_type_and_mismatched_optical_identity_never_authorize_operations() {
        let mut bad = Discovery::new("auto");
        run(&mut bad, identity(2), true);
        assert!(bad.blocked());
        let mut identity = identity(3);
        identity.cwmp_serial = "other-child".into();
        let mut mismatch = Discovery::new("auto");
        mismatch.tick("test-child", identity, 0, true, |_| {
            panic!("identity mismatch must not probe another child")
        });
        assert!(mismatch.blocked());
    }

    #[test]
    fn known_and_forced_profiles_require_complete_current_wan_evidence() {
        for requested in ["auto", "hl202-cu"] {
            for incomplete_object in [false, true] {
                let mut probe = Discovery::new(requested);
                probe.identity = Some(Identity {
                    optical_vendor: "H3CT".into(),
                    optical_serial: "H3CT00000004".into(),
                    cwmp_serial: "H3CT00000004".into(),
                    product_class: "HL202-DU-C".into(),
                    software_version: String::new(),
                    hardware_version: String::new(),
                });
                let firmware = "InternetGatewayDevice.DeviceInfo.SoftwareVersion";
                probe.names.insert(firmware.into(), false);
                probe
                    .values
                    .insert(firmware.into(), (ScalarType::String, "v1".into()));
                if incomplete_object {
                    probe.names.insert("InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.WANIPConnection.1.Enable".into(), true);
                }
                probe.resolve();
                assert!(probe.blocked());
                assert!(!probe.ready());
                assert!(probe.selected().is_none());
            }
        }
    }
}
