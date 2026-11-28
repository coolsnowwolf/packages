pub(crate) fn observe(
    sources: &mut Vec<crate::ipc::SubDevice>,
    observation: crate::ipc::SubDevice,
    protected: &std::collections::BTreeSet<String>,
) {
    if let Some(current) = sources
        .iter_mut()
        .find(|source| source.id == observation.id)
    {
        *current = observation;
        return;
    }
    if sources.len() >= 64 {
        let candidate = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| !protected.contains(&source.id))
            .min_by_key(|(_, source)| source.last_seen)
            .map(|(index, _)| index);
        match candidate {
            Some(index) => {
                sources.remove(index);
            }
            None => return,
        }
    }
    sources.push(observation);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telemetry_sources_bound_preserves_active_identity() {
        let mut sources = (0..64)
            .map(|n| crate::ipc::SubDevice {
                id: format!("{n:012X}"),
                last_seen: n,
                payload_bytes: 1,
            })
            .collect();
        let protected = std::collections::BTreeSet::from(["000000000000".to_string()]);
        observe(
            &mut sources,
            crate::ipc::SubDevice {
                id: "FFFFFFFFFFFF".into(),
                last_seen: 100,
                payload_bytes: 2,
            },
            &protected,
        );
        assert_eq!(sources.len(), 64);
        assert!(sources.iter().any(|source| source.id == "000000000000"));
        assert!(!sources.iter().any(|source| source.id == "000000000001"));
        assert!(sources.iter().any(|source| source.id == "FFFFFFFFFFFF"));
    }
}
