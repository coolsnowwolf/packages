use super::super::redirect_net::nft as net_nft;
use std::net::Ipv4Addr;

pub(super) const BRIDGE_TABLE: &str = "fttr_mqtt_redirect";
pub(super) const IP_TABLE: &str = "fttr_mqtt_redirect";

pub(super) use net_nft::format_mac;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RedirectRule {
    pub interface: String,
    pub client_mac: [u8; 6],
    pub client_ip: Ipv4Addr,
    pub target_gw: Ipv4Addr,
    pub broker_ip: Ipv4Addr,
    pub bridge_mac: [u8; 6],
}

impl RedirectRule {
    pub(super) fn bridge_rule_expr(&self) -> String {
        format!(
            "iifname \"{}\" ether saddr {} ip saddr {} ip daddr {} tcp dport 1883 meta pkttype set host ether daddr set {} counter",
            self.interface,
            format_mac(self.client_mac),
            self.client_ip,
            self.target_gw,
            format_mac(self.bridge_mac)
        )
    }

    pub(super) fn ip_rule_expr(&self) -> String {
        format!(
            "ip saddr {} ip daddr {} tcp dport 1883 counter dnat to {}:1883",
            self.client_ip, self.target_gw, self.broker_ip
        )
    }
}

pub(super) fn render_apply_script(rules: &[RedirectRule]) -> Result<String, String> {
    if rules.is_empty() {
        return Ok(net_nft::render_delete_script(Some(BRIDGE_TABLE), IP_TABLE));
    }
    let mut bridge_rules = Vec::with_capacity(rules.len());
    let mut ip_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        if !net_nft::valid_interface(&rule.interface) {
            return Err(format!("invalid ONU interface name: {}", rule.interface));
        }
        bridge_rules.push(rule.bridge_rule_expr());
        ip_rules.push(rule.ip_rule_expr());
    }
    Ok(net_nft::render_prerouting_script(
        Some(BRIDGE_TABLE),
        IP_TABLE,
        &bridge_rules,
        &ip_rules,
    ))
}

pub(super) fn apply(rules: &[RedirectRule]) -> Result<(), String> {
    let script = render_apply_script(rules)?;
    net_nft::run_nft_batch(&script)
}

pub(super) fn remove() -> Result<(), String> {
    net_nft::run_nft_batch(&net_nft::render_delete_script(Some(BRIDGE_TABLE), IP_TABLE))
}

pub(super) fn tables_present() -> Result<(bool, bool), String> {
    net_nft::tables_present(Some(BRIDGE_TABLE), IP_TABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_exact_per_device_bridge_and_ip_rules() {
        let rule = RedirectRule {
            interface: "fttr127".into(),
            client_mac: [0x1c, 0x94, 0x68, 0x5d, 0xf9, 0x98],
            client_ip: Ipv4Addr::new(192, 0, 2, 244),
            target_gw: Ipv4Addr::new(192, 0, 2, 1),
            broker_ip: Ipv4Addr::new(192, 0, 2, 44),
            bridge_mac: [0xd4, 0xa0, 0x29, 0x03, 0x20, 0x94],
        };
        let script = render_apply_script(&[rule]).unwrap();
        assert!(script
            .contains("table bridge fttr_mqtt_redirect\ndelete table bridge fttr_mqtt_redirect"));
        assert!(script.contains("iifname \"fttr127\" ether saddr 1c:94:68:5d:f9:98 ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 meta pkttype set host ether daddr set d4:a0:29:03:20:94 counter"));
        assert!(script.contains("ip saddr 192.0.2.244 ip daddr 192.0.2.1 tcp dport 1883 counter dnat to 192.0.2.44:1883"));
    }

    #[test]
    fn rejects_invalid_interface_names_and_detects_tables() {
        let bad = RedirectRule {
            interface: "fttr1\"; drop".into(),
            client_mac: [2, 0, 0, 0, 0, 1],
            client_ip: Ipv4Addr::new(192, 0, 2, 2),
            target_gw: Ipv4Addr::new(192, 0, 2, 1),
            broker_ip: Ipv4Addr::new(192, 0, 2, 3),
            bridge_mac: [2, 0, 0, 0, 0, 2],
        };
        assert!(render_apply_script(&[bad]).is_err());

        let parsed = serde_json::json!({
            "nftables": [
                {"metainfo": {"version": "1.1.7"}},
                {"table": {"family": "inet", "name": "fw4"}},
                {"table": {"family": "bridge", "name": "fttr_mqtt_redirect"}},
                {"table": {"family": "ip", "name": "fttr_mqtt_redirect"}}
            ]
        });
        assert_eq!(
            net_nft::parse_tables_present(&parsed, Some(BRIDGE_TABLE), IP_TABLE),
            (true, true)
        );
    }
}
