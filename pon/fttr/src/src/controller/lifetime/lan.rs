//! Read the actual logical LAN address, independently of its UCI protocol.
use std::net::Ipv4Addr;
use std::time::Duration;

fn select_address(status: &crate::platform::openwrt::netifd::InterfaceStatus) -> Option<Ipv4Addr> {
    if !status.up {
        return None;
    }
    status.ipv4_address.iter().find_map(|entry| {
        let address = entry.address.parse::<Ipv4Addr>().ok()?;
        crate::broker::lan_addresses(&address.to_string()).ok()?;
        Some(address)
    })
}

pub(super) async fn address() -> Result<Option<Ipv4Addr>, String> {
    crate::runtime::spawn_blocking(|| {
        let mut bus = openwrt_ubus::Ubus::connect(None).map_err(|error| error.to_string())?;
        let status = crate::platform::openwrt::NetifdClient::new(&mut bus).status("lan")?;
        Ok(select_address(&status))
    })
    .await
    .map_err(|error| format!("LAN status task failed: {error}"))?
}

// An ubus failure is not evidence that the address disappeared. In contrast,
// a successful status without an address is a real loss of the listener.
pub(super) fn changed(applied: Ipv4Addr, status: &Result<Option<Ipv4Addr>, String>) -> bool {
    matches!(status, Ok(current) if *current != Some(applied))
}

pub(super) async fn wait_for_address() -> Ipv4Addr {
    eprintln!("[*] Waiting for LAN IPv4 for MQTT");
    loop {
        match address().await {
            Ok(Some(address)) => return address,
            Ok(None) => (),
            Err(error) => eprintln!("[-] LAN IPv4 unavailable: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::openwrt::netifd::{InterfaceStatus, Ipv4AddressEntry};

    #[test]
    fn selects_usable_runtime_address_only_when_interface_is_up() {
        let mut status = InterfaceStatus {
            up: false,
            ipv4_address: vec![
                Ipv4AddressEntry {
                    address: "0.0.0.0".into(),
                    mask: 0,
                },
                Ipv4AddressEntry {
                    address: "192.0.2.10".into(),
                    mask: 24,
                },
            ],
            ..Default::default()
        };
        assert_eq!(select_address(&status), None);
        status.up = true;
        assert_eq!(select_address(&status), Some("192.0.2.10".parse().unwrap()));
        status.ipv4_address.clear();
        assert_eq!(select_address(&status), None);
    }

    #[test]
    fn restart_only_for_confirmed_address_loss_or_change() {
        let applied = "192.0.2.10".parse().unwrap();
        assert!(!changed(applied, &Err("ubus unavailable".into())));
        assert!(!changed(applied, &Ok(Some(applied))));
        assert!(changed(applied, &Ok(None)));
        assert!(changed(applied, &Ok(Some("192.0.2.11".parse().unwrap()))));
    }
}
