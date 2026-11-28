pub(super) fn observed(state: &super::State, device: &str, prefix: &str) -> Option<bool> {
    state
        .tasks
        .values()
        .rev()
        .filter(|task| {
            task.device == device
                && task.method == "GetParameterValues"
                && task.state == super::TaskState::Completed
                && super::now().saturating_sub(
                    state
                        .results
                        .get(&task.id)
                        .and_then(|i| i.completed_at)
                        .unwrap_or(task.sent.unwrap_or(0)),
                ) <= 90
        })
        .find_map(|task| {
            state
                .results
                .get(&task.id)
                .and_then(|i| {
                    // Parent-scoped fields (for example X_CU_VLAN) need the
                    // descendant connections' evidence after ACK drops the body.
                    i.management_services
                        .iter()
                        .filter(|(root, _)| root.starts_with(prefix))
                        .map(|(_, management)| *management)
                        .reduce(|a, b| a || b)
                })
                .or_else(|| is_management(task.result.as_deref()?, prefix))
        })
}

pub(super) fn service_evidence(
    value: &crate::cwmp::response::Data,
) -> std::collections::BTreeMap<String, bool> {
    let mut evidence = std::collections::BTreeMap::new();
    for row in value.rows().into_iter().flatten() {
        let Some(name) = row["name"].as_str() else {
            continue;
        };
        if name.len() > 512 || !(name.ends_with("ServiceList") || name.ends_with("SERVICELIST")) {
            continue;
        }
        let Some((prefix, _)) = name.rsplit_once('.') else {
            continue;
        };
        let management = row["value"]
            .as_str()
            .is_none_or(|value| crate::provisioning::contains_service_token(value, "TR069"));
        evidence
            .entry(format!("{prefix}."))
            .and_modify(|v| *v |= management)
            .or_insert(management);
        if evidence.len() >= 64 {
            break;
        }
    }
    evidence
}

pub(super) fn is_management(result: &crate::cwmp::response::Data, prefix: &str) -> Option<bool> {
    let Some(rows) = result.rows() else {
        return Some(true);
    };
    let row = rows.iter().find(|p| {
        p["name"].as_str().is_some_and(|name| {
            name.starts_with(prefix)
                && (name.ends_with("ServiceList") || name.ends_with("SERVICELIST"))
        })
    })?;
    Some(
        row["value"]
            .as_str()
            .is_none_or(|v| crate::provisioning::contains_service_token(v, "TR069")),
    )
}
