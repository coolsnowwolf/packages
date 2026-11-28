use super::super::redirect_net::nft as net_nft;
use std::net::Ipv4Addr;

pub(super) const BRIDGE_TABLE: &str = "fttr_acs_redirect";
pub(super) const IP_TABLE: &str = "fttr_acs_redirect";
pub(super) const LAN_REDIRECT_MARK: &str = "0x00ac5001";

pub(super) fn target_expr(ip: Ipv4Addr, prefix: u8) -> String {
    if prefix >= 32 {
        ip.to_string()
    } else {
        format!("{ip}/{prefix}")
    }
}

pub(super) fn render_lan_bridge_rule(
    interface: &str,
    target_ip: Ipv4Addr,
    target_prefix: u8,
    target_port: u16,
    bridge_mac: [u8; 6],
) -> Result<String, String> {
    if !net_nft::valid_interface(interface) {
        return Err(format!("invalid ONU interface name: {interface}"));
    }
    Ok(format!(
        "iifname \"{interface}\" ip daddr {} tcp dport {target_port} meta pkttype set host ether daddr set {} meta mark set {LAN_REDIRECT_MARK} counter",
        target_expr(target_ip, target_prefix),
        net_nft::format_mac(bridge_mac),
    ))
}

pub(super) fn render_lan_ip_rule(
    target_ip: Ipv4Addr,
    target_prefix: u8,
    target_port: u16,
    acs_ip: Ipv4Addr,
    acs_port: u16,
) -> String {
    format!(
        "iifname \"br-lan\" meta mark {LAN_REDIRECT_MARK} ip daddr {} tcp dport {target_port} counter dnat to {acs_ip}:{acs_port}",
        target_expr(target_ip, target_prefix),
    )
}

pub(super) fn render_management_ip_rule(
    source_ip: Option<Ipv4Addr>,
    target_ip: Ipv4Addr,
    target_prefix: u8,
    target_port: u16,
    mgmt_ip: Ipv4Addr,
    mgmt_port: u16,
) -> String {
    match source_ip {
        Some(src) => format!(
            "iifname \"br-tr069\" ip saddr {src} ip daddr {} tcp dport {target_port} counter dnat to {mgmt_ip}:{mgmt_port}",
            target_expr(target_ip, target_prefix),
        ),
        None => format!(
            "iifname \"br-tr069\" ip daddr {} tcp dport {target_port} counter dnat to {mgmt_ip}:{mgmt_port}",
            target_expr(target_ip, target_prefix),
        ),
    }
}

pub(super) fn render_apply_script(bridge_rules: &[String], ip_rules: &[String]) -> String {
    net_nft::render_prerouting_script(Some(BRIDGE_TABLE), IP_TABLE, bridge_rules, ip_rules)
}

pub(super) fn apply(bridge_rules: &[String], ip_rules: &[String]) -> Result<(), String> {
    let script = render_apply_script(bridge_rules, ip_rules);
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
    fn renders_fttr_only_lan_and_management_redirect_rules() {
        let bridge_rule = render_lan_bridge_rule(
            "fttr1",
            Ipv4Addr::new(10, 20, 30, 0),
            24,
            7547,
            [0x02, 0x00, 0x00, 0x00, 0x00, 0xaa],
        )
        .unwrap();
        let lan_ip_rule = render_lan_ip_rule(
            Ipv4Addr::new(10, 20, 30, 0),
            24,
            7547,
            Ipv4Addr::new(192, 0, 2, 44),
            7547,
        );
        let mgmt_ip_rule = render_management_ip_rule(
            Some(Ipv4Addr::new(192, 0, 2, 101)),
            Ipv4Addr::new(10, 20, 30, 40),
            32,
            8080,
            Ipv4Addr::new(192, 0, 2, 1),
            7547,
        );
        let mgmt_all_rule = render_management_ip_rule(
            None,
            Ipv4Addr::new(10, 20, 30, 41),
            32,
            7547,
            Ipv4Addr::new(192, 0, 2, 1),
            7547,
        );
        let script =
            render_apply_script(&[bridge_rule], &[lan_ip_rule, mgmt_ip_rule, mgmt_all_rule]);
        assert!(script.contains("table bridge fttr_acs_redirect"));
        assert!(script.contains("iifname \"fttr1\" ip daddr 10.20.30.0/24 tcp dport 7547 meta pkttype set host ether daddr set 02:00:00:00:00:aa meta mark set 0x00ac5001 counter"));
        assert!(script.contains("iifname \"br-lan\" meta mark 0x00ac5001 ip daddr 10.20.30.0/24 tcp dport 7547 counter dnat to 192.0.2.44:7547"));
        assert!(script.contains("iifname \"br-tr069\" ip saddr 192.0.2.101 ip daddr 10.20.30.40 tcp dport 8080 counter dnat to 192.0.2.1:7547"));
        assert!(script.contains("iifname \"br-tr069\" ip daddr 10.20.30.41 tcp dport 7547 counter dnat to 192.0.2.1:7547"));
    }

    #[test]
    fn rejects_invalid_interface_names_in_bridge_rule() {
        assert!(render_lan_bridge_rule(
            "fttr1;drop",
            Ipv4Addr::new(10, 0, 0, 1),
            32,
            7547,
            [0x02, 0, 0, 0, 0, 1],
        )
        .is_err());
    }
}
