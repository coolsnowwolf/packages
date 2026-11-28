//! Admission and management-plane write protection, shared by enqueue/dispatch.
use super::{readback, soap, State};
use serde_json::Value;

pub(super) fn protect_unresolved(state: &State, device: &str, method: &str) -> Result<(), String> {
    if super::task::is_write(method)
        && state
            .tasks
            .values()
            .any(|t| t.device == device && t.unresolved_write())
    {
        return Err("unresolved CWMP write; transaction-aware recovery required".into());
    }
    Ok(())
}

pub(super) fn protect_management(
    state: &State,
    device: &str,
    method: &str,
    args: &Value,
) -> Result<(), String> {
    const ROOT: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.";
    protect_unresolved(state, device, method)?;
    let paths: Vec<&str> = match method {
        "SetParameterValues" => args["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["name"].as_str())
            .collect(),
        "AddObject" | "DeleteObject" => vec![args["object"].as_str().unwrap_or("")],
        _ => return Ok(()),
    };
    if crate::omci_auth::cwmp_epoch(device).is_none() {
        return Err("OMCI authentication does not authorize this write".into());
    }
    if method == "SetParameterValues"
        && args["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| {
                p["name"].as_str().is_some_and(|n| {
                    n.starts_with("InternetGatewayDevice.ManagementServer.")
                        || n.starts_with("Device.ManagementServer.")
                        || ((n.ends_with("ServiceList") || n.ends_with("SERVICELIST"))
                            && crate::provisioning::contains_service_token(
                                p["value"].as_str().unwrap_or(""),
                                "TR069",
                            ))
                })
            })
    {
        return Err("TR069 management is owned by OMCI".into());
    }
    for path in paths {
        if method == "AddObject" && path == ROOT {
            continue;
        }
        if !path.starts_with("InternetGatewayDevice.WANDevice.") {
            continue;
        }
        let parent = path
            .strip_prefix(ROOT)
            .and_then(|p| p.split('.').next())
            .and_then(|p| p.parse::<u32>().ok())
            .ok_or("protected WAN object")?;
        let parts: Vec<_> = path.strip_prefix(ROOT).unwrap().split('.').collect();
        let connection = parts
            .get(1)
            .filter(|v| matches!(**v, "WANIPConnection" | "WANPPPConnection"))
            .zip(parts.get(2).and_then(|n| n.parse::<u32>().ok()))
            .map(|(kind, n)| (*kind, n));
        if !crate::provisioning::owns_business_wan(device, parent, connection) {
            return Err("WAN is not an authorized editable business connection".into());
        }
        if method == "AddObject" {
            continue;
        }
        if connection.is_none() && crate::provisioning::protected_wan_parent(device, parent) {
            return Err("WAN parent contains a protected fallback or management connection".into());
        }
        let prefix = connection
            .map(|(kind, n)| format!("{ROOT}{parent}.{kind}.{n}."))
            .unwrap_or_else(|| format!("{ROOT}{parent}."));
        if readback::observed(state, device, &prefix).is_none_or(|management| management) {
            return Err("WAN write requires a fresh non-TR069 service readback".into());
        }
    }
    Ok(())
}

pub(super) fn validate(method: &str, args: &Value) -> Result<(), String> {
    let path = |v: &Value| v.as_str().is_some_and(soap::valid_path);
    match method {
        "GetRPCMethods" => (),
        "AddObject" | "DeleteObject"
            if path(&args["object"])
                && args["object"].as_str().is_some_and(|s| {
                    s.starts_with("InternetGatewayDevice.")
                        && s.ends_with('.')
                        && s.split('.').count() >= 5
                }) => {}
        "GetParameterNames" if path(&args["path"]) && args["next_level"].is_boolean() => (),
        "GetParameterValues"
            if args["names"].as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.len() <= 64
                    && a.iter().all(|v| {
                        path(v) && v != "" && !v.as_str().is_some_and(|s| s.ends_with('.'))
                    })
            }) => {}
        "SetParameterValues" => {
            soap::set_parameters(args).map_err(|e| e.to_string())?;
        }
        _ => return Err("invalid method or parameter arguments".into()),
    }
    Ok(())
}
