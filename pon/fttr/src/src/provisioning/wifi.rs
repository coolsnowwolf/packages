//! WLAN lifecycle is separate from parameter diffs. Inventory, actual returned
//! instances and readback are authoritative; an acknowledged Delete is not proof.
use super::model::{ItemKind, ItemOutcome, TaskOutcome, WifiPendingAction, WifiPhase};
use super::{acs, config::Device};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
pub const ROOT: &str = "InternetGatewayDevice.LANDevice.1.WLANConfiguration.";
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub owned: BTreeSet<u32>,
    pub pending: Option<WifiPendingAction>,
    pub pending_instance: Option<u32>,
}
fn path(id: &str) -> String {
    super::runtime_store::Record::Wifi.path(id)
}

#[cfg(test)]
mod tests {
    use super::super::{
        config::{Item, Parameter},
        model::ParameterKind,
        RPC,
    };
    use super::*;
    fn device() -> Device {
        Device {
            id: "child".into(),
            optical_sn: "H3CT12345678".into(),
            cwmp_id: "id".into(),
            omci_adapter: "hl202".into(),
            bootstrap_adapter: "manual-acs".into(),
            provisioning_adapter: "hl202-cu".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            wifi_cleanup: "delete".into(),
            wifi_preserve: BTreeSet::new(),
            items: vec![Item {
                wlan_intent: None,
                binding_intent: None,
                id: "wifi".into(),
                kind: ItemKind::Ssid,
                service_type: String::new(),
                parameters: vec![Parameter {
                    name: format!("{ROOT}1.SSID"),
                    value: "wanted".into(),
                    kind: ParameterKind::String,
                    secret: false,
                }],
                guard: None,
                blocked: None,
                wan_ref: None,
                auto_instance: None,
                wan_intent: None,
            }],
        }
    }
    fn reply(instances: &[u32]) -> Value {
        json!({"state":"completed","result":{"parameters":instances.iter().map(|n|json!({"name":format!("{ROOT}{n}."),"writable":"1"})).collect::<Vec<_>>()}})
    }
    fn responses(v: Vec<Value>) {
        RPC.with(|r| *r.borrow_mut() = (v.into(), Vec::new()));
    }
    fn initialized(d: &Device) -> Reconciler {
        let mut r = Reconciler::new(d, Registry::default(), true);
        r.stage = WifiPhase::Inventory;
        r.task = Some(1);
        r
    }
    #[test]
    fn deletes_only_extras_and_verifies_absence() {
        let d = device();
        let mut r = initialized(&d);
        responses(vec![
            reply(&[1, 2]),
            json!({"id":2}),
            json!({"state":"completed"}),
            json!({"id":3}),
            reply(&[1]),
        ]);
        assert!(!r.tick(&d, true, true, true, true));
        assert!(!r.tick(&d, true, true, true, true));
        assert!(r.tick(&d, true, true, true, true));
        assert_eq!(r.results["2"]["state"], "deleted");
        assert!(r.existing.contains(&1));
        RPC.with(|r| assert_eq!(r.borrow().1[1]["args"]["object"], format!("{ROOT}2.")));
    }
    #[test]
    fn plan_never_writes_and_failure_prevents_cleanup() {
        let d = device();
        let mut r = initialized(&d);
        responses(vec![reply(&[1, 2]), json!({"busy":false})]);
        assert!(!r.tick(&d, false, true, true, false));
        assert!(r.tick(&d, false, true, true, false));
        assert_eq!(r.results["2"]["state"], "drift");
        RPC.with(|r| assert!(r.borrow().1.iter().all(|v| v["op"] != "queue")));
        let mut r = initialized(&d);
        responses(vec![reply(&[1, 2])]);
        assert!(r.tick(&d, true, true, true, false));
        assert_eq!(r.results["inventory"]["state"], "blocked");
    }
    #[test]
    fn acknowledged_delete_is_not_success_without_absence() {
        let d = device();
        let mut r = initialized(&d);
        responses(vec![
            reply(&[1, 2]),
            json!({"id":2}),
            json!({"state":"completed"}),
            json!({"id":3}),
            reply(&[1, 2]),
        ]);
        for _ in 0..3 {
            r.tick(&d, true, true, true, true);
        }
        assert_eq!(r.results["2"]["state"], "unknown");
        assert_eq!(r.registry.pending, Some(WifiPendingAction::Deleting));
    }
    #[test]
    fn returned_slot_is_verified_before_further_allocation_and_unknown_is_not_replayed() {
        let mut d = device();
        d.items[0].parameters[0].name = format!("{ROOT}6.SSID");
        let mut r = initialized(&d);
        responses(vec![
            reply(&[1, 5]),
            json!({"id":2}),
            json!({"state":"completed","result":{"instance_number":"2"}}),
            json!({"id":3}),
            reply(&[1, 2, 5]),
            json!({"id":4}),
            json!({"state":"unknown"}),
        ]);
        for _ in 0..4 {
            r.tick(&d, true, true, false, false);
        }
        assert!(r.registry.owned.contains(&2));
        assert!(!r.existing.contains(&6));
        assert_eq!(r.results["6"]["state"], "unknown");
        let calls = RPC.with(|r| r.borrow().1.len());
        assert!(r.tick(&d, true, true, false, false));
        RPC.with(|r| assert_eq!(r.borrow().1.len(), calls));
    }
    #[test]
    fn preserve_policy_removes_owned_only_and_keep_list_wins() {
        let mut d = device();
        d.wifi_cleanup = "preserve".into();
        d.wifi_preserve.insert(3);
        let mut r = initialized(&d);
        r.registry.owned.extend([2, 3]);
        responses(vec![reply(&[1, 2, 3, 4]), json!({"id":2})]);
        r.tick(&d, true, true, true, true);
        RPC.with(|r| assert_eq!(r.borrow().1[1]["args"]["object"], format!("{ROOT}2.")));
    }
    #[test]
    fn matched_inventory_apply_performs_no_lifecycle_write() {
        let d = device();
        let mut r = initialized(&d);
        responses(vec![reply(&[1])]);
        assert!(r.tick(&d, true, true, true, true));
        assert!(r.results.is_empty());
        RPC.with(|r| assert!(r.borrow().1.iter().all(|v| v["op"] != "queue")));
    }
    #[test]
    fn already_disabled_extra_is_not_written_again() {
        let mut d = device();
        d.wifi_cleanup = "disable".into();
        let mut r = initialized(&d);
        responses(vec![
            reply(&[1, 2]),
            json!({"id":2}),
            json!({"state":"completed","result":{"parameters":[{"name":format!("{ROOT}2.Enable"),"value":"0"}]}}),
        ]);
        assert!(!r.tick(&d, true, true, true, true));
        assert!(r.tick(&d, true, true, true, true));
        assert_eq!(r.results["2"]["state"], "disabled");
        RPC.with(|r| {
            assert!(r
                .borrow()
                .1
                .iter()
                .all(|v| v["method"] != "SetParameterValues"))
        });
    }
    #[test]
    fn reused_instance_response_requires_inventory_and_does_not_loop() {
        let mut d = device();
        d.items[0].parameters[0].name = format!("{ROOT}6.SSID");
        let mut r = initialized(&d);
        responses(vec![
            reply(&[1, 2, 5]),
            json!({"id":2}),
            json!({"state":"completed","result":{"instance_number":"2"}}),
            json!({"id":3}),
            reply(&[1, 2, 5]),
        ]);
        assert!(!r.tick(&d, true, true, false, false));
        assert!(!r.tick(&d, true, true, false, false));
        assert!(r.tick(&d, true, true, false, false));
        assert_eq!(r.results["2"]["state"], "blocked");
        assert!(r.registry.pending.is_none());
        assert!(r.registry.owned.is_empty());
        r.block_unresolved(&mut d);
        assert!(d.items[0].blocked.is_some());
        RPC.with(|r| {
            assert_eq!(
                r.borrow()
                    .1
                    .iter()
                    .filter(|v| v["method"] == "AddObject")
                    .count(),
                1
            )
        });
    }
    #[test]
    fn malformed_inventory_never_erases_ownership_or_confirms_delete() {
        let d = device();
        let valid = json!({"name":format!("{ROOT}1."),"writable":"1"});
        for result in [
            json!({}),
            json!({"parameters":null}),
            json!({"parameters":[valid.clone(), {"name":"wrong.2.","writable":"1"}]}),
            json!({"parameters":[valid.clone(), valid.clone()]}),
            json!({"parameters":[{"name":format!("{ROOT}2.SSID"),"writable":"1"}]}),
            json!({"parameters":[{"name":format!("{ROOT}0."),"writable":"1"}]}),
            json!({"parameters":[{"name":format!("{ROOT}2."),"value":"1"}]}),
            json!({"parameters":[{"name":format!("{ROOT}2.")}]}),
        ] {
            for stage in [WifiPhase::Inventory, WifiPhase::VerifyDelete] {
                let mut r = initialized(&d);
                r.stage = stage;
                r.registry.owned.insert(2);
                r.registry.pending = Some(WifiPendingAction::Deleting);
                r.registry.pending_instance = Some(2);
                r.current = Some(2);
                responses(vec![json!({"state":"completed","result":result})]);
                assert!(r.tick(&d, true, true, true, true));
                assert!(r.registry.owned.contains(&2));
                assert_eq!(r.registry.pending, Some(WifiPendingAction::Deleting));
                assert_eq!(r.results["2"]["state"], "unknown");
                RPC.with(|rpc| assert!(rpc.borrow().1.iter().all(|v| v["op"] != "queue")));
            }
        }
        let data = serde_json::from_value(json!({"parameters":[]})).unwrap();
        assert!(inventory(&data).unwrap().0.is_empty());
    }

    #[test]
    fn fresh_inventory_and_plan_do_not_settle_interrupted_delete() {
        let d = device();
        for apply in [false, true] {
            let mut r = initialized(&d);
            r.registry.owned.insert(2);
            r.registry.pending = Some(WifiPendingAction::Deleting);
            r.registry.pending_instance = Some(2);
            responses(vec![reply(&[1])]);
            assert!(r.tick(&d, apply, true, true, true));
            assert!(r.registry.owned.contains(&2));
            assert_eq!(r.registry.pending, Some(WifiPendingAction::Deleting));
        }
    }

    #[test]
    fn save_retries_directory_barrier_after_rename_and_identical_content() {
        use super::super::storage::test_io::{self, Point};
        let d = device();
        let r = Reconciler::new(&d, Registry::default(), true);
        test_io::fail_next(Point::DirectorySync);
        assert!(r.save(&d.cwmp_id).is_err());
        assert!(std::path::Path::new(&path(&d.cwmp_id)).exists());
        test_io::events();
        test_io::fail_next(Point::DirectorySync);
        assert!(r.save(&d.cwmp_id).is_err());
        assert_eq!(
            test_io::events(),
            vec![Point::FileSync, Point::DirectorySync]
        );
        r.save(&d.cwmp_id).unwrap();
        let events = test_io::events();
        assert_eq!(events[0], Point::FileSync);
        assert!(events[1..].iter().all(|p| *p == Point::DirectorySync));
        assert!(events.len() > 2);
    }
}
pub fn load(id: &str) -> Result<Registry, String> {
    let text = match super::storage::read_to_string(path(id)) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => return Err(format!("read Wi-Fi ownership: {e}")),
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid Wi-Fi ownership: {e}"))?;
    if v["cwmp_id"] != id {
        return Err("Wi-Fi ownership identity mismatch".into());
    }
    serde_json::from_value(v["registry"].clone())
        .map_err(|e| format!("invalid Wi-Fi registry: {e}"))
}
#[cfg(not(test))]
pub fn reset(id: &str) -> Result<(), String> {
    super::storage::remove_registry(&path(id))
}
pub fn instance(name: &str) -> Option<u32> {
    name.strip_prefix(ROOT)?.split('.').next()?.parse().ok()
}
struct CompleteInventory(BTreeSet<u32>);

fn inventory(v: &crate::cwmp::response::Data) -> Result<CompleteInventory, &'static str> {
    let rows = v.rows().ok_or("missing WLAN inventory rows")?;
    let mut instances = BTreeSet::new();
    for row in rows {
        if !matches!(row, crate::cwmp::response::Row::Info { .. })
            || !matches!(row["writable"].as_str(), Some("0" | "1" | "false" | "true"))
        {
            return Err("invalid WLAN inventory row type");
        }
        let token = row["name"]
            .as_str()
            .and_then(|name| name.strip_prefix(ROOT))
            .and_then(|name| name.strip_suffix('.'))
            .ok_or("out of scope WLAN inventory row")?;
        let n = token
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0 && n.to_string() == token)
            .ok_or("invalid WLAN inventory instance")?;
        if !instances.insert(n) {
            return Err("duplicate WLAN inventory instance");
        }
    }
    // An explicit empty ParameterList from a completed RPC is valid. A missing
    // list, malformed row or filtered subset is never evidence of absence.
    Ok(CompleteInventory(instances))
}
#[derive(Clone)]
pub struct Reconciler {
    submission: Option<crate::cwmp::transport::Pending<WifiPhase>>,
    pub registry: Registry,
    pub results: BTreeMap<String, Value>,
    pub stage: WifiPhase,
    pub desired: BTreeSet<u32>,
    existing: BTreeSet<u32>,
    task: Option<u64>,
    current: Option<u32>,
    cleanup: bool,
    policy: String,
    preserve: BTreeSet<u32>,
    prepared: bool,
    additions: u32,
    reused: bool,
}
impl Reconciler {
    pub(super) fn progress(&self) -> (WifiPhase, Option<u64>, bool, Option<u32>, u32) {
        (
            self.stage,
            self.task,
            self.prepared,
            self.current,
            self.additions,
        )
    }
    pub fn new(d: &Device, registry: Registry, cleanup: bool) -> Self {
        let desired: BTreeSet<_> = d
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Ssid)
            .flat_map(|i| i.parameters.iter().filter_map(|p| instance(&p.name)))
            .collect();
        let skip = desired.is_empty()
            && (!cleanup || (d.wifi_cleanup == "preserve" && registry.owned.is_empty()));
        Self {
            registry,
            results: BTreeMap::new(),
            stage: if skip {
                WifiPhase::Done
            } else {
                WifiPhase::Waiting
            },
            desired,
            existing: BTreeSet::new(),
            task: None,
            submission: None,
            current: None,
            cleanup,
            policy: d.wifi_cleanup.clone(),
            preserve: d.wifi_preserve.clone(),
            prepared: skip,
            additions: 0,
            reused: false,
        }
    }
    pub fn save(&self, id: &str) -> Result<(), String> {
        let text = json!({"cwmp_id":id,"registry":self.registry}).to_string();
        super::storage::write_atomic(&path(id), text.as_bytes(), true)
            .map_err(|e| format!("persist Wi-Fi ownership: {e}"))
    }
    fn fail(&mut self, state: ItemOutcome, reason: &str) {
        self.results.insert(
            self.current
                .map(|n| n.to_string())
                .unwrap_or_else(|| "inventory".into()),
            json!({"state":state,"reason":reason,"task_id":self.task}),
        );
        self.stage = WifiPhase::Done;
        self.prepared = true;
        self.task = None;
        self.submission = None;
    }
    pub fn block_unresolved(&self, d: &mut Device) {
        for item in d.items.iter_mut().filter(|i| i.kind == ItemKind::Ssid) {
            if item
                .parameters
                .iter()
                .filter_map(|p| instance(&p.name))
                .any(|n| !self.existing.contains(&n))
            {
                item.blocked = Some(
                    "desired WLAN is absent; allocation not completed (see wifi_lifecycle)".into(),
                );
            }
        }
    }
    fn queue(&mut self, d: &Device, method: &str, args: Value, stage: WifiPhase) {
        self.submission = Some(crate::cwmp::transport::Pending {
            method: method.into(),
            args,
            stage,
        });
        self.submit(d);
    }
    fn submit(&mut self, d: &Device) {
        if self.save(&d.cwmp_id).is_err() {
            self.fail(
                ItemOutcome::Blocked,
                "cannot persist Wi-Fi lifecycle intent",
            );
            return;
        }
        let pending = self.submission.as_ref().unwrap();
        let v = acs(Command::queue(
            &d.cwmp_id,
            &pending.method,
            pending.args.clone(),
        ));
        if let Some(id) = v["id"].as_u64() {
            self.task = Some(id);
            self.stage = self.submission.take().unwrap().stage;
        } else if crate::cwmp::transport::backpressure(&v) {
            self.task = None;
        } else {
            self.submission = None;
            self.registry.pending = None;
            self.registry.pending_instance = None;
            self.fail(ItemOutcome::Failed, "cannot queue Wi-Fi RPC");
        }
    }
    pub fn tick(
        &mut self,
        d: &Device,
        apply: bool,
        reachable: bool,
        finish: bool,
        verified: bool,
    ) -> bool {
        if self.stage == WifiPhase::Done {
            return true;
        }
        if self.prepared && !finish {
            return true;
        }
        if !reachable {
            return false;
        }
        if self.submission.is_some() {
            self.submit(d);
            return false;
        }
        if self.task.is_none()
            && acs(Command::Busy {
                device: d.cwmp_id.clone(),
            })["busy"]
                == true
        {
            return false;
        }
        if self.stage == WifiPhase::Waiting {
            self.queue(
                d,
                "GetParameterNames",
                json!({"path":ROOT,"next_level":true}),
                WifiPhase::Inventory,
            );
            return false;
        }
        if let Some(task) = self.task {
            let v = acs(Command::Task { id: task });
            let state = TaskOutcome::from_task(&v);
            if matches!(state, TaskOutcome::Queued | TaskOutcome::Sent) {
                return false;
            }
            if !matches!(
                state,
                TaskOutcome::Completed | TaskOutcome::AcceptedPendingApply
            ) {
                if matches!(state, TaskOutcome::Fault | TaskOutcome::Expired) {
                    self.registry.pending = None;
                    self.registry.pending_instance = None;
                }
                self.fail(
                    if state == TaskOutcome::Unknown {
                        ItemOutcome::Unknown
                    } else {
                        ItemOutcome::Failed
                    },
                    "Wi-Fi RPC failed/unknown; no automatic write replay",
                );
                return true;
            }
            let complete_inventory = if matches!(
                self.stage,
                WifiPhase::Inventory | WifiPhase::VerifyAdd | WifiPhase::VerifyDelete
            ) {
                match inventory(v.result()) {
                    Ok(CompleteInventory(instances)) => Some(instances),
                    Err(reason) => {
                        self.fail(
                            if self.registry.pending.is_some() {
                                ItemOutcome::Unknown
                            } else {
                                ItemOutcome::Blocked
                            },
                            reason,
                        );
                        return true;
                    }
                }
            } else {
                None
            };
            self.task = None;
            match self.stage {
                WifiPhase::Inventory => {
                    self.existing = complete_inventory.unwrap();
                    if self.registry.pending.is_some() {
                        self.fail(ItemOutcome::Unknown,"interrupted Wi-Fi lifecycle; fresh inventory does not prove the old RPC stopped");
                        return true;
                    }
                    if apply {
                        self.registry.owned.retain(|n| self.existing.contains(n));
                    }
                }
                WifiPhase::Adding => {
                    let n = v.result()["instance_number"]
                        .as_str()
                        .and_then(|n| n.parse::<u32>().ok())
                        .filter(|n| *n > 0);
                    let Some(n) = n else {
                        self.fail(
                            ItemOutcome::Unknown,
                            "AddObject did not return a WLAN instance",
                        );
                        return true;
                    };
                    self.reused = self.existing.contains(&n);
                    if !self.reused {
                        self.registry.owned.insert(n);
                    }
                    self.registry.pending_instance = Some(n);
                    self.current = Some(n);
                    self.queue(
                        d,
                        "GetParameterNames",
                        json!({"path":ROOT,"next_level":true}),
                        WifiPhase::VerifyAdd,
                    );
                    return false;
                }
                WifiPhase::VerifyAdd => {
                    let before = self.existing.clone();
                    self.existing = complete_inventory.unwrap();
                    let n = self.current.unwrap();
                    if !self.existing.contains(&n) {
                        self.fail(
                            ItemOutcome::Unknown,
                            "AddObject acknowledged but WLAN is absent",
                        );
                        return true;
                    }
                    if self.reused {
                        if before == self.existing {
                            self.registry.pending = None;
                            self.registry.pending_instance = None;
                            self.fail(ItemOutcome::Blocked,"firmware reused existing WLAN instance; requested slot cannot be allocated with this contract");
                        } else {
                            self.fail(ItemOutcome::Unknown,"AddObject returned existing instance but inventory changed; inspect before retry");
                        }
                        return true;
                    }
                    self.registry.pending = None;
                    self.registry.pending_instance = None;
                    self.results.insert(n.to_string(),json!({"state":"created","instance":n,"task_id":task,"reason":"AddObject returned instance, existence verified"}));
                }
                WifiPhase::Deleting => {
                    self.queue(
                        d,
                        "GetParameterNames",
                        json!({"path":ROOT,"next_level":true}),
                        WifiPhase::VerifyDelete,
                    );
                    return false;
                }
                WifiPhase::VerifyDelete => {
                    self.existing = complete_inventory.unwrap();
                    let n = self.current.unwrap();
                    if self.existing.contains(&n) {
                        self.fail(
                            ItemOutcome::Unknown,
                            "DeleteObject acknowledged but WLAN still exists",
                        );
                        return true;
                    }
                    self.registry.owned.remove(&n);
                    self.registry.pending = None;
                    self.registry.pending_instance = None;
                    self.results.insert(n.to_string(),json!({"state":"deleted","instance":n,"task_id":task,"reason":"WLAN absence verified"}));
                }
                WifiPhase::ReadingDisable | WifiPhase::VerifyingDisable => {
                    let name = format!("{ROOT}{}.Enable", self.current.unwrap());
                    let disabled = v
                        .result()
                        .rows()
                        .into_iter()
                        .flatten()
                        .find(|p| p["name"] == name)
                        .and_then(|p| p["value"].as_str());
                    let Some(value) = disabled else {
                        self.fail(ItemOutcome::Failed, "missing WLAN Enable readback");
                        return true;
                    };
                    if matches!(value, "0" | "false") {
                        let n = self.current.unwrap();
                        self.results.insert(n.to_string(),json!({"state":"disabled","instance":n,"task_id":task,"reason":"Enable=0 verified"}));
                        self.registry.pending = None;
                        self.registry.pending_instance = None;
                    } else if self.stage == WifiPhase::VerifyingDisable {
                        self.fail(
                            ItemOutcome::Unknown,
                            "disable accepted/unknown but Enable still true; no replay",
                        );
                        return true;
                    } else if !apply {
                        self.results.insert(self.current.unwrap().to_string(),json!({"state":"drift","reason":"unconfigured WLAN enabled; apply will disable"}));
                    } else {
                        self.registry.pending = Some(WifiPendingAction::Disabling);
                        self.registry.pending_instance = self.current;
                        self.queue(
                            d,
                            "SetParameterValues",
                            json!({"parameters":[{"name":name,"value":"0","kind":"boolean"}]}),
                            WifiPhase::Disabling,
                        );
                        return false;
                    }
                }
                WifiPhase::Disabling => {
                    self.queue(
                        d,
                        "GetParameterValues",
                        json!({"names":[format!("{ROOT}{}.Enable",self.current.unwrap())]}),
                        WifiPhase::VerifyingDisable,
                    );
                    return false;
                }
                WifiPhase::Waiting | WifiPhase::Next | WifiPhase::Done => (),
            }
        }
        self.stage = WifiPhase::Next;
        if !self.prepared {
            if let Some(n) = self.desired.difference(&self.existing).next().copied() {
                self.current = Some(n);
                if !apply {
                    self.results.insert(n.to_string(),json!({"state":"drift","reason":"desired WLAN absent; apply will allocate"}));
                    self.prepared = true;
                } else if n > 8 || self.additions >= 8 {
                    self.fail(
                        ItemOutcome::Blocked,
                        "cannot allocate requested instance within verified adapter slots 1..8",
                    );
                    return true;
                } else {
                    // Only use actual returned slots. Some firmware reuses an existing
                    // slot; verification above blocks that contract instead of looping.
                    self.additions += 1;
                    self.registry.pending = Some(WifiPendingAction::Adding);
                    self.registry.pending_instance = None;
                    self.queue(d, "AddObject", json!({"object":ROOT}), WifiPhase::Adding);
                    return false;
                }
            } else {
                self.prepared = true;
            }
        }
        if !finish {
            return true;
        }
        if !verified && apply {
            self.fail(
                ItemOutcome::Blocked,
                "desired SSIDs must verify before removing other WLANs",
            );
            return true;
        }
        if self.cleanup {
            let candidate = self.existing.iter().copied().find(|n| {
                !self.desired.contains(n)
                    && !self.preserve.contains(n)
                    && (self.policy != "preserve" || self.registry.owned.contains(n))
                    && !self.results.get(&n.to_string()).is_some_and(|r| {
                        matches!(
                            ItemOutcome::from_result(r),
                            Some(ItemOutcome::Deleted | ItemOutcome::Disabled | ItemOutcome::Drift)
                        )
                    })
            });
            if let Some(n) = candidate {
                self.current = Some(n);
                if self.policy == "disable" {
                    self.queue(
                        d,
                        "GetParameterValues",
                        json!({"names":[format!("{ROOT}{n}.Enable")]}),
                        WifiPhase::ReadingDisable,
                    );
                    return false;
                }
                if !apply {
                    self.results.insert(n.to_string(),json!({"state":"drift","reason":"unconfigured WLAN present; apply will delete"}));
                    return false;
                }
                self.registry.pending = Some(WifiPendingAction::Deleting);
                self.registry.pending_instance = Some(n);
                self.queue(
                    d,
                    "DeleteObject",
                    json!({"object":format!("{ROOT}{n}.")}),
                    WifiPhase::Deleting,
                );
                return false;
            }
        }
        self.stage = WifiPhase::Done;
        true
    }
}
use crate::cwmp::transport::Command;
