//! Mandatory filtering precedes auxiliary signature scoring; ties stay unresolved.
use super::{
    discovery::{Evidence, Resolution, ScalarType},
    registry, vendor,
};
fn coherent(evidence: &[Evidence], path: &str, kind: ScalarType) -> bool {
    let rows: Vec<_> = evidence.iter().filter(|e| e.path == path).collect();
    !rows.is_empty()
        && rows
            .iter()
            .all(|e| e.writable == Some(true) && e.kind == Some(kind))
}
pub fn match_wan(evidence: &[Evidence], parent: &str, connection: &str) -> Resolution {
    let mut candidates = Vec::new();
    for contract in vendor::candidates() {
        let w = contract.wan;
        let complete = coherent(
            evidence,
            &w.service.path(parent, connection),
            w.service.kind,
        ) && coherent(evidence, &w.vlan.path(parent, connection), w.vlan.kind)
            && coherent(
                evidence,
                &w.priority.path(parent, connection),
                w.priority.kind,
            )
            && w.enabled
                .is_none_or(|f| coherent(evidence, &f.path(parent, connection), f.kind));
        if !complete {
            continue;
        }
        let mut score = 0;
        for profile in registry::available()
            .into_iter()
            .filter(|p| p.vendor == contract.vendor)
        {
            if profile.wan.binding.is_some_and(|f| {
                coherent(evidence, &f.path(parent, connection), ScalarType::String)
            }) {
                score += 3;
            }
            if profile.wan.cloud_cpe.is_some_and(|f| {
                coherent(evidence, &f.path(parent, connection), ScalarType::Boolean)
            }) {
                score += 2;
            }
        }
        candidates.push((score, contract));
    }
    let Some(highest) = candidates.iter().map(|(s, _)| *s).max() else {
        return Resolution::Incomplete;
    };
    let best: Vec<_> = candidates
        .iter()
        .filter(|(s, _)| *s == highest)
        .map(|(_, c)| *c)
        .collect();
    match best.as_slice() {
        [c] => Resolution::Matched(c),
        _ => Resolution::Ambiguous(best.iter().map(|c| c.id).collect()),
    }
}
