//! Independent runtime readback of the planned management resources.
use super::*;
use std::time::{Duration, Instant};

pub(super) fn verify_firewall(
    d: &Desired,
    previous: Option<&Desired>,
    rules: &Value,
) -> Result<(), String> {
    let entries = rules["nftables"]
        .as_array()
        .ok_or("missing nftables runtime inventory")?;
    let chain_rules = |chain: &str| -> Vec<&Value> {
        entries
            .iter()
            .filter_map(|e| e.get("rule"))
            .filter(|r| r["chain"] == chain)
            .collect()
    };
    let rejects = |rule: &Value| {
        rule["expr"].as_array().is_some_and(|expr| {
            expr.iter().any(|e| {
                e.get("reject").is_some()
                    || e["jump"]["target"]
                        .as_str()
                        .is_some_and(|t| t.contains("reject"))
            })
        })
    };
    let forward = chain_rules("forward_fttr_tr069");
    let input = chain_rules("input_fttr_tr069");
    if !forward.iter().any(|r| rejects(r))
        || !input.iter().any(|r| rejects(r))
        || forward.iter().any(|r| {
            r["expr"].as_array().is_some_and(|expr| {
                expr.iter().any(|e| {
                    e.get("accept").is_some()
                        || e["jump"]["target"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("accept_to_"))
                })
            })
        })
    {
        return Err("management zone input/forward rejection is not installed".into());
    }
    let allowed = |rule: &Value, name: &str, proto: &str, port: u16| {
        rule["comment"].as_str().is_some_and(|c| c.contains(name))
            && rule["expr"].as_array().is_some_and(|expr| {
                expr.iter().any(|e| e.get("accept").is_some())
                    && expr.iter().any(|e| {
                        e["match"]["left"]["payload"]["protocol"] == proto
                            && e["match"]["left"]["payload"]["field"] == "dport"
                            && e["match"]["right"] == port
                    })
                    && expr.iter().any(|e| {
                        e["match"]["left"]["meta"]["key"] == "nfproto"
                            && e["match"]["right"] == "ipv4"
                    })
            })
    };
    let allowed_icmp = |rule: &Value| {
        rule["comment"]
            .as_str()
            .is_some_and(|c| c.contains("FTTR management ICMP"))
            && rule["expr"].as_array().is_some_and(|expr| {
                expr.iter().any(|e| e.get("accept").is_some())
                    && expr.iter().any(|e| {
                        e["match"]["left"]["payload"]["protocol"] == "icmp"
                            && e["match"]["left"]["payload"]["field"] == "type"
                            && (e["match"]["right"] == "echo-request"
                                || e["match"]["right"] == 8
                                || e["match"]["right"]["set"].as_array().is_some_and(|s| {
                                    s.iter().any(|v| v == "echo-request" || v == 8)
                                }))
                    })
            })
    };
    if (d.acs_enabled
        && !input
            .iter()
            .any(|r| allowed(r, "FTTR management ACS", "tcp", d.port)))
        || (d.dhcp
            && !input
                .iter()
                .any(|r| allowed(r, "FTTR management DHCP", "udp", 67)))
        || (d.enabled && !input.iter().any(|r| allowed_icmp(r)))
    {
        return Err("management DHCP/ACS/ICMP exceptions are missing or have the wrong protocol/port/family".into());
    }
    if input.iter().any(|r| {
        r["expr"]
            .as_array()
            .is_some_and(|expr| expr.iter().any(|e| e.get("accept").is_some()))
            && !((d.acs_enabled && allowed(r, "FTTR management ACS", "tcp", d.port))
                || (d.dhcp && allowed(r, "FTTR management DHCP", "udp", 67))
                || (d.enabled && allowed_icmp(r))
                || previous.is_some_and(|p| {
                    p.acs_enabled && allowed(r, "FTTR previous management ACS", "tcp", p.port)
                }))
    }) {
        return Err("unexpected management input acceptance is installed".into());
    }
    for interface in ["br-tr069".to_string()] {
        for chain in ["input", "forward"] {
            let target = format!("{chain}_fttr_tr069");
            if !chain_rules(chain).iter().any(|r| {
                r["expr"].as_array().is_some_and(|expr| {
                    expr.iter().any(|e| e["jump"]["target"] == target)
                        && expr.iter().any(|e| {
                            e["match"]["left"]["meta"]["key"] == "iifname"
                                && (e["match"]["right"] == interface
                                    || e["match"]["right"]["set"]
                                        .as_array()
                                        .is_some_and(|s| s.iter().any(|v| v == &interface)))
                        })
                })
            }) {
                return Err(
                    "management interface is not dispatched to its isolated firewall zone".into(),
                );
            }
        }
    }
    Ok(())
}

pub(super) fn dhcp_range_matches(d: &Desired, interface: &str, text: &str) -> bool {
    let mask = u32::MAX << (32 - d.prefix);
    let network = u32::from(d.address.parse::<Ipv4Addr>().unwrap()) & mask;
    let start = Ipv4Addr::from(network + d.start).to_string();
    let end = Ipv4Addr::from(network + d.start + d.limit - 1).to_string();
    let mask = Ipv4Addr::from(mask).to_string();
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("dhcp-range="))
        .any(|r| {
            let fields: Vec<_> = r.split(',').collect();
            fields
                .iter()
                .any(|tag| *tag == format!("set:{interface}") || *tag == format!("tag:{interface}"))
                && fields.windows(4).any(|v| {
                    v == [
                        start.as_str(),
                        end.as_str(),
                        mask.as_str(),
                        d.lease.as_str(),
                    ]
                })
        })
}

pub(super) fn verify_dhcp(d: &Desired, interface: &str) -> Result<(), String> {
    if !d.enabled || !d.dhcp {
        return Ok(());
    }
    let mut ubus = openwrt_ubus::Ubus::connect(None)
        .map_err(|e| format!("cannot connect to ubus for DHCP verification: {e}"))?;
    let mut client = crate::platform::openwrt::ServiceClient::new(&mut ubus);
    let services = client.list("dnsmasq")?;
    let instances = services["dnsmasq"]["instances"]
        .as_object()
        .ok_or("DHCP service instances are missing")?;
    for instance in instances.values().filter(|i| i["running"] == true) {
        let Some(args) = instance["command"].as_array() else {
            continue;
        };
        let path = args
            .windows(2)
            .find_map(|a| {
                (a[0] == "-C" || a[0] == "--conf-file")
                    .then(|| a[1].as_str())
                    .flatten()
            })
            .or_else(|| {
                args.iter()
                    .filter_map(Value::as_str)
                    .find_map(|a| a.strip_prefix("--conf-file="))
            });
        let Some(path) = path else {
            continue;
        };
        if std::fs::metadata(path).is_ok_and(|m| m.len() <= 1024 * 1024) {
            if let Ok(text) = std::fs::read_to_string(path) {
                if dhcp_range_matches(d, interface, &text) {
                    return Ok(());
                }
            }
        }
    }
    Err("the running DHCP service does not contain the desired management pool".into())
}

pub(super) fn verify_bridge(d: &Desired, previous: Option<&Desired>) -> Result<(), String> {
    let root = "/sys/class/net/br-tr069";
    if std::fs::read_to_string(format!("{root}/bridge/vlan_filtering"))
        .map_err(|_| "management bridge is missing")?
        .trim()
        != "0"
    {
        return Err("management bridge VLAN filtering must be disabled".into());
    }
    let mut ports: Vec<String> =
        resources(d, previous)["network.fttr_tr069_bridge"].options["ports"].clone();
    ports.sort();
    let mut actual = std::fs::read_dir(format!("{root}/brif"))
        .map_err(|_| "management bridge ports are unavailable")?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "cannot inspect management bridge ports")?;
    actual.sort();
    if actual != ports {
        return Err("management bridge runtime membership differs from the VLAN list".into());
    }
    let links: Value = serde_json::from_str(&run("ip", &["-j", "-d", "link", "show"])?)
        .map_err(|_| "invalid VLAN link inventory")?;
    let links = links.as_array().ok_or("missing VLAN links")?;
    for port in ports {
        let vlan = port
            .strip_prefix("br-lan.")
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or("invalid managed VLAN name")?;
        if !links.iter().any(|l| {
            l["ifname"] == port
                && l["master"] == "br-tr069"
                && l["link"] == "br-lan"
                && l["linkinfo"]["info_kind"] == "vlan"
                && l["linkinfo"]["info_data"]["id"] == vlan
        }) {
            return Err(format!("management VLAN runtime differs: {port}"));
        }
    }
    Ok(())
}

pub(super) fn verify_network(d: &Desired, previous: Option<&Desired>) -> Result<(), String> {
    if !plan_resources(d, previous, &discover()?)?.is_empty() {
        return Err("management UCI readback differs from the plan".into());
    }
    let mut ubus = openwrt_ubus::Ubus::connect(None)
        .map_err(|e| format!("cannot connect to ubus for network verification: {e}"))?;
    let mut client = crate::platform::openwrt::NetifdClient::new(&mut ubus);

    if !d.enabled {
        let dump = client.dump()?;
        if dump
            .interface
            .iter()
            .any(|i| i.interface == "fttr_tr069" && i.up)
        {
            return Err("disabled management interface is still up".into());
        }
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(status) = client.status("fttr_tr069") {
            if status.up
                && status
                    .ipv4_address
                    .iter()
                    .any(|a| a.address == d.address && a.mask == d.prefix)
            {
                break;
            }
        }
        if Instant::now() >= deadline {
            return Err("management interface address did not become ready".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let rules: Value = serde_json::from_str(&run("nft", &["-j", "list", "table", "inet", "fw4"])?)
        .map_err(|_| "invalid nftables runtime status")?;
    verify_firewall(d, previous, &rules)?;
    verify_bridge(d, previous)?;
    verify_dhcp(d, "fttr_tr069")?;
    if let Some(p) = previous.filter(|p| p.address != d.address || p.prefix != d.prefix) {
        let status = client.status("fttr_tr069_previous")?;
        if !status.up
            || !status
                .ipv4_address
                .iter()
                .any(|i| i.address == p.address && i.mask == p.prefix)
        {
            return Err("previous management path was not retained".into());
        }
        verify_dhcp(p, "fttr_tr069_previous")?;
    }
    Ok(())
}
