//! Read-only discovery state machine. Requests are handed to the existing ACS
//! queue by its caller; repeated polling never issues overlapping requests.
use super::{
    discovery::{match_wan, Evidence, Resolution, ScalarType},
    path::WanObjects,
    vendor,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Names { path: String },
    Values { names: Vec<String> },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Names,
    AwaitingNames,
    Values,
    AwaitingValues,
    Complete,
    Failed,
}

pub struct Probe {
    parent: String,
    connection: String,
    pub stage: Stage,
    expected: BTreeSet<String>,
    evidence: BTreeMap<String, Evidence>,
    values: BTreeMap<String, String>,
}
impl Probe {
    pub fn new(objects: &WanObjects) -> Result<Self, String> {
        let (parent, connection) = objects.paths()?;
        let mut expected = BTreeSet::new();
        for c in vendor::candidates() {
            let w = c.wan;
            for f in [Some(w.service), Some(w.vlan), Some(w.priority), w.enabled]
                .into_iter()
                .flatten()
            {
                expected.insert(f.path(&parent, &connection));
            }
        }
        Ok(Self {
            parent,
            connection,
            stage: Stage::Names,
            expected,
            evidence: BTreeMap::new(),
            values: BTreeMap::new(),
        })
    }
    pub fn take_request(&mut self) -> Option<Request> {
        match self.stage {
            Stage::Names => {
                self.stage = Stage::AwaitingNames;
                Some(Request::Names {
                    path: self.parent.clone(),
                })
            }
            Stage::Values => {
                self.stage = Stage::AwaitingValues;
                Some(Request::Values {
                    names: self.evidence.keys().cloned().collect(),
                })
            }
            _ => None,
        }
    }
    pub fn names_result(&mut self, rows: &[(String, bool)]) -> Result<(), String> {
        if self.stage != Stage::AwaitingNames {
            return Err("unexpected Names response".into());
        }
        if rows.len() > 4096 {
            self.fail();
            return Err("parameter discovery exceeds bound".into());
        }
        for (path, writable) in rows {
            if !self.expected.contains(path) {
                continue;
            }
            if self
                .evidence
                .get(path)
                .is_some_and(|e| e.writable != Some(*writable))
            {
                self.fail();
                return Err("conflicting writable evidence".into());
            }
            self.evidence.insert(
                path.clone(),
                Evidence {
                    path: path.clone(),
                    writable: Some(*writable),
                    kind: None,
                },
            );
        }
        self.stage = if self.evidence.is_empty() {
            Stage::Complete
        } else {
            Stage::Values
        };
        Ok(())
    }
    pub fn values_result(&mut self, rows: &[(String, ScalarType, String)]) -> Result<(), String> {
        if self.stage != Stage::AwaitingValues {
            return Err("unexpected Values response".into());
        }
        let mut seen = BTreeSet::new();
        for (path, kind, value) in rows {
            let Some(evidence) = self.evidence.get_mut(path) else {
                self.fail();
                return Err("unexpected discovery value".into());
            };
            if !seen.insert(path.clone()) {
                self.fail();
                return Err("duplicate discovery value".into());
            }
            evidence.kind = Some(*kind);
            self.values.insert(path.clone(), value.clone());
        }
        if seen.len() != self.evidence.len() {
            self.fail();
            return Err("incomplete discovery values".into());
        }
        self.stage = Stage::Complete;
        Ok(())
    }
    pub fn fail(&mut self) {
        self.stage = Stage::Failed;
        self.evidence.clear();
        self.values.clear();
    }
    pub fn resolution(&self) -> Resolution {
        if self.stage != Stage::Complete {
            return Resolution::Incomplete;
        }
        match_wan(
            &self.evidence.values().cloned().collect::<Vec<_>>(),
            &self.parent,
            &self.connection,
        )
    }
    /// Management protection is checked across known service fields even if
    /// vendor selection is ambiguous, incomplete, or not write-verified.
    pub fn management_present(&self) -> bool {
        vendor::candidates().iter().any(|c| {
            self.values
                .get(&c.wan.service.path(&self.parent, &self.connection))
                .is_some_and(|v| {
                    super::codec::service_tokens(v).any(|t| t.eq_ignore_ascii_case("TR069"))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::path::ConnectionKind;
    use super::*;
    #[test]
    fn probe_is_readonly_bounded_and_requires_complete_typed_readback() {
        let objects = WanObjects {
            parent: 3,
            connection: 2,
            kind: ConnectionKind::Ip,
        };
        let (p, c) = objects.paths().unwrap();
        let mut probe = Probe::new(&objects).unwrap();
        assert!(matches!(probe.take_request(), Some(Request::Names { .. })));
        assert!(probe.take_request().is_none());
        let w = vendor::hw::BASE.wan;
        let names = [w.service, w.vlan, w.priority].map(|f| (f.path(&p, &c), true));
        probe.names_result(&names).unwrap();
        assert!(matches!(probe.take_request(), Some(Request::Values { .. })));
        assert!(probe.take_request().is_none());
        probe
            .values_result(&[
                (
                    names[0].0.clone(),
                    ScalarType::String,
                    "TR069_INTERNET".into(),
                ),
                (names[1].0.clone(), ScalarType::UnsignedInt, "0".into()),
                (names[2].0.clone(), ScalarType::UnsignedInt, "0".into()),
            ])
            .unwrap();
        assert!(matches!(probe.resolution(), Resolution::Matched(c) if c.vendor == "hw"));
        assert!(probe.management_present());
        assert!(probe.take_request().is_none());
        probe.fail();
        assert!(matches!(probe.resolution(), Resolution::Incomplete));
    }
}
