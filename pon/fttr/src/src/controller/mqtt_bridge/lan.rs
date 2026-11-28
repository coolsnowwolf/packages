pub(super) use super::super::redirect_net::lan::{
    inspect_lan_context, origin_str, parse_mac_hex12, same_subnet, LanContext,
};
use super::evidence::{Origin, ServerOrigins};

pub(super) fn server_origins(ctx: &LanContext) -> ServerOrigins {
    let mut origins = ServerOrigins::new();
    if ctx.local_lan_dhcp {
        if let Some(ip) = ctx.lan_ip {
            origins.insert(ip, Origin::Local);
        }
    }
    if ctx.origin == Origin::Upstream {
        if let Some(srv) = ctx.upstream_dhcp_server {
            origins.insert(srv, Origin::Upstream);
        }
    }
    origins
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn local_origin_maps_lan_ip_as_local_dhcp_server() {
        let lan = LanContext {
            origin: Origin::Local,
            lan_ip: Some(Ipv4Addr::new(192, 0, 2, 1)),
            local_lan_dhcp: true,
            ..Default::default()
        };
        let origins = server_origins(&lan);
        assert_eq!(
            origins.get(&Ipv4Addr::new(192, 0, 2, 1)),
            Some(&Origin::Local)
        );
    }
}
