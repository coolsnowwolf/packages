//! Per-ONU IEEE 802.1ad (TPID 0x88a8) VPORT Linux VLAN subinterfaces over shared `fttr0`.
//!
//! FSL maps upstream/downstream NNI outer VLAN IDs using `VID = gem_port_id - NNI_GEMPORT_OFFSET`.
//! Programming the FSL NNI TPID to `0x88a8` lets the standard Linux `8021q` stack demultiplex
//! each ONU onto its own `fttr1..fttr128` netdev while preserving inner `0x8100` business/TR-069 VLANs.
use super::active_resources;
use crate::platform::openwrt::command::run_sync;
use std::{
    fs, io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// Enough room for 1500B payload + 4B inner 802.1Q tag + 4B outer 802.1ad VPORT tag.
pub const NNI_MTU: u16 = 1508;
const DEFAULT_BRIDGE: &str = "br-lan";
const PORT_ALIAS: &str = "fttrd:fiberhome-vport";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
// Serialize all VPORT netlink/ip-link mutations so concurrent PortGate watchdogs
// and optical quiesce do not stampede kernel rtnl_lock / synchronize_net().
static VPORT_LOCK: Mutex<()> = Mutex::new(());

pub fn gem_to_vid(gem_id: u16, offset: u16) -> io::Result<u16> {
    let vid = gem_id.checked_sub(offset).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("GEM {gem_id} is below NNI offset {offset}"),
        )
    })?;
    if !(1..=4094).contains(&vid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("mapped NNI VLAN ID {vid} is outside 1..=4094"),
        ));
    }
    Ok(vid)
}

fn valid_interface_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn configured_bridge() -> String {
    let Ok(view) = crate::config::load() else {
        return DEFAULT_BRIDGE.into();
    };
    let name = view
        .option("offload", "bridge")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if valid_interface_name(&name) {
        name
    } else {
        DEFAULT_BRIDGE.into()
    }
}

fn run_ip_link(args: &[&str]) -> io::Result<()> {
    let mut command = vec!["link"];
    command.extend_from_slice(args);
    run_sync("ip", &command, COMMAND_TIMEOUT)
        .map(|_| ())
        .map_err(|error| {
            io::Error::new(error.kind(), format!("ip link {}: {error}", args.join(" ")))
        })
}

fn matching_vlan_details(details: &str, vlan: &str, nni: &str, vid: u16) -> bool {
    let tokens: Vec<_> = details.split_whitespace().collect();
    let has_protocol = tokens.windows(2).any(|w| w == ["protocol", "802.1ad"]);
    let has_vid = tokens
        .windows(2)
        .any(|w| w[0] == "id" && w[1].parse::<u16>() == Ok(vid));
    let parent = vlan
        .lines()
        .any(|line| line.trim() == format!("Device: {nni}"));
    has_protocol && has_vid && parent
}

fn matching_proc_vlan(vlan: &str, iface: &str, nni: &str, vid: u16) -> bool {
    let expected_vid = vid.to_string();
    let has_header = vlan.lines().next().is_some_and(|line| {
        let tokens: Vec<_> = line.split_whitespace().collect();
        tokens.first() == Some(&iface)
            && tokens
                .windows(2)
                .any(|w| w[0] == "VID:" && w[1] == expected_vid)
    });
    let parent = vlan
        .lines()
        .any(|line| line.trim() == format!("Device: {nni}"));
    has_header && parent
}

fn verify_existing_port_fast(iface: &str, nni: &str, vid: u16) -> io::Result<()> {
    let vlan = fs::read_to_string(format!("/proc/net/vlan/{iface}"))?;
    let alias = fs::read_to_string(format!("/sys/class/net/{iface}/ifalias"))?;
    if alias.trim() == PORT_ALIAS && matching_proc_vlan(&vlan, iface, nni, vid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{iface} conflicts with expected {nni} VLAN {vid}"),
        ))
    }
}

fn is_port_flags_down(iface: &str) -> io::Result<bool> {
    let path = format!("/sys/class/net/{iface}/flags");
    match fs::read_to_string(path) {
        Ok(flags) => {
            let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
                .map_err(io::Error::other)?;
            Ok(flags & 1 == 0)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn verify_existing_port(iface: &str, nni: &str, vid: u16) -> io::Result<()> {
    let output = run_sync("ip", &["-d", "link", "show", "dev", iface], COMMAND_TIMEOUT)?;
    let vlan = fs::read_to_string(format!("/proc/net/vlan/{iface}"))?;
    let alias = fs::read_to_string(format!("/sys/class/net/{iface}/ifalias"))?;
    if alias.trim() == PORT_ALIAS && matching_vlan_details(&output, &vlan, nni, vid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{iface} conflicts with expected {nni} 802.1ad VLAN {vid}"),
        ))
    }
}

fn attach_bridge(iface: &str, bridge: &str) -> io::Result<()> {
    if !Path::new(&format!("/sys/class/net/{bridge}/bridge")).is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("bridge {bridge} is unavailable"),
        ));
    }
    run_ip_link(&["set", iface, "master", bridge])
}

fn verify_no_link_children(net: &Path, iface: &str, index: &str) -> io::Result<()> {
    for entry in fs::read_dir(net)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == iface {
            continue;
        }
        // /sys/class/net also contains ordinary files, e.g. bonding_masters.
        // Follow interface symlinks, while tolerating an interface disappearing.
        match fs::metadata(entry.path()) {
            Ok(metadata) if !metadata.is_dir() => continue,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        let link = fs::read_to_string(entry.path().join("iflink"))?;
        if link.trim() == index.trim() {
            return Err(io::Error::other(format!(
                "{iface} has dependent interface {}",
                name.to_string_lossy()
            )));
        }
    }
    Ok(())
}

fn verify_no_dependencies(iface: &str, bridge: &str) -> io::Result<()> {
    let network = fs::read_to_string("/etc/config/network")?;
    if network_references_interface(&network, iface) {
        return Err(io::Error::other(format!(
            "{iface} is referenced by network configuration"
        )));
    }
    let path = format!("/sys/class/net/{iface}");
    let index = fs::read_to_string(format!("{path}/ifindex"))?;
    verify_no_link_children(Path::new("/sys/class/net"), iface, &index)?;
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(upper) = name.strip_prefix("upper_") {
            if upper != bridge {
                return Err(io::Error::other(format!(
                    "{iface} has dependent interface {upper}"
                )));
            }
        }
    }
    Ok(())
}

fn network_references_interface(network: &str, iface: &str) -> bool {
    network
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .any(|word| {
            word == iface
                || word
                    .strip_prefix(iface)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
}

pub fn prepare_nni_and_ports(nni: &str, gem_offset: u16, stopping: &AtomicBool) -> io::Result<()> {
    let _guard = VPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Reject a foreign or legacy port before changing the NNI or any sibling.
    for entry in active_resources().allocations {
        // Same responsiveness rule as the creation loop below: this pass also
        // shells out once per port.
        if stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopping"));
        }
        let Some(iface) = entry.interface else {
            continue;
        };
        let vid = gem_to_vid(entry.gem_id, gem_offset)?;
        if Path::new(&format!("/sys/class/net/{iface}")).exists() {
            verify_existing_port(iface, nni, vid)?;
        }
    }
    // First inspect every owned out-of-range port; foreign interfaces are never removed.
    let bridge = configured_bridge();
    let mut stale = Vec::new();
    for entry in &super::RESOURCES.allocations[active_resources().allocations.len()..] {
        if stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopping"));
        }
        let iface = entry.interface.expect("AN4475 port mapping");
        let path = format!("/sys/class/net/{iface}");
        if !Path::new(&path).exists() {
            continue;
        }
        let alias = fs::read_to_string(format!("{path}/ifalias"))?;
        if alias.trim() != PORT_ALIAS {
            continue;
        }
        verify_existing_port(iface, nni, gem_to_vid(entry.gem_id, gem_offset)?)?;
        verify_no_dependencies(iface, &bridge)?;
        stale.push(iface);
    }
    for iface in stale {
        // Recheck immediately before deletion to narrow dependency races.
        let entry = super::RESOURCES
            .allocations
            .iter()
            .find(|entry| entry.interface == Some(iface))
            .expect("AN4475 port mapping");
        verify_existing_port(iface, nni, gem_to_vid(entry.gem_id, gem_offset)?)?;
        verify_no_dependencies(iface, &bridge)?;
        run_ip_link(&["delete", iface])?;
    }
    let mtu = NNI_MTU.to_string();
    run_ip_link(&["set", nni, "mtu", &mtu, "up"])?;
    let carrier_path = format!("/sys/class/net/{nni}/carrier");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut carrier_up = false;
    while Instant::now() < deadline && !stopping.load(Ordering::Acquire) {
        if fs::read_to_string(&carrier_path).is_ok_and(|val| val.trim() == "1") {
            carrier_up = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !carrier_up {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{nni} carrier did not come up within 5s"),
        ));
    }

    for entry in active_resources().allocations {
        // 128 ports × several bounded `ip` calls each: keep a shutdown request
        // responsive instead of finishing the whole loop. The next start
        // re-runs this idempotent preparation for the remaining ports.
        if stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopping"));
        }
        let Some(iface) = entry.interface else {
            continue;
        };
        let vid = gem_to_vid(entry.gem_id, gem_offset)?;
        let vid_str = vid.to_string();
        if Path::new(&format!("/sys/class/net/{iface}")).exists() {
            verify_existing_port(iface, nni, vid)?;
        } else {
            run_ip_link(&[
                "add", "link", nni, "name", iface, "type", "vlan", "protocol", "802.1ad", "id",
                &vid_str,
            ])?;
            run_ip_link(&["set", iface, "alias", PORT_ALIAS])?;
        }
        run_ip_link(&["set", iface, "mtu", "1500", "down"])?;
        attach_bridge(iface, &bridge)?;
    }
    Ok(())
}

fn set_port_up_locked(onu_id: u8, up: bool) -> io::Result<()> {
    let iface = active_resources().interface(onu_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("ONU {onu_id} has no assigned VPORT interface"),
        )
    })?;
    if !up && is_port_flags_down(iface)? {
        return Ok(());
    }
    if !Path::new(&format!("/sys/class/net/{iface}")).exists() {
        if !up {
            // No interface means no carrier. Teardown need not wait for a
            // concurrently removed VLAN to reappear before releasing ownership.
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("VPORT interface {iface} does not exist"),
        ));
    }
    let entry = active_resources().onu(onu_id)?;
    let vid = gem_to_vid(
        entry.gem_id,
        super::initialization::defaults::DEFAULT_NNI_GEMPORT_OFFSET,
    )?;
    if up {
        verify_existing_port(iface, "fttr0", vid)?;
        let bridge = configured_bridge();
        attach_bridge(iface, &bridge)?;
        run_ip_link(&["set", iface, "up"])
    } else {
        // Verify VPORT ownership via sysfs + procfs without spawning `ip -d link show`.
        verify_existing_port_fast(iface, "fttr0", vid)?;
        run_ip_link(&["set", iface, "down"])
    }
}

pub fn set_port_up(onu_id: u8, up: bool) -> io::Result<()> {
    let _guard = VPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_port_up_locked(onu_id, up)
}

pub fn bring_all_ports_down() -> io::Result<()> {
    let _guard = VPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut errors = Vec::new();
    for entry in active_resources().allocations {
        if let Err(error) = set_port_up_locked(entry.onu_id, false) {
            errors.push(format!("ONU {}: {error}", entry.onu_id));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gem_to_vid_maps_fsl_slots_and_rejects_out_of_range() {
        assert_eq!(gem_to_vid(0x100, 255).unwrap(), 1);
        assert_eq!(gem_to_vid(0x101, 255).unwrap(), 2);
        assert_eq!(gem_to_vid(0x10f, 255).unwrap(), 16);
        assert_eq!(gem_to_vid(0x17f, 255).unwrap(), 128);
        assert!(gem_to_vid(255, 255).is_err());
        assert!(gem_to_vid(254, 255).is_err());
        assert!(gem_to_vid(4095, 0).is_err());
    }

    #[test]
    fn interface_name_validation_rejects_traversal_and_spaces() {
        assert!(valid_interface_name("br-lan"));
        assert!(valid_interface_name("br_fttr.1"));
        assert!(!valid_interface_name(""));
        assert!(!valid_interface_name("../br-lan"));
        assert!(!valid_interface_name("br lan"));
        assert!(!valid_interface_name("0123456789abcdef"));
    }

    #[test]
    fn stale_port_network_references_include_vlan_children_but_not_other_slots() {
        assert!(network_references_interface(
            "list ports 'fttr128'",
            "fttr128"
        ));
        assert!(network_references_interface(
            "option device 'fttr128.46'",
            "fttr128"
        ));
        assert!(!network_references_interface(
            "list ports 'fttr128'",
            "fttr12"
        ));
    }

    #[test]
    fn dependency_scan_skips_non_interface_files_and_rejects_children() {
        let root =
            std::env::temp_dir().join(format!("fttr-vport-dependencies-{}", std::process::id()));
        fs::create_dir_all(root.join("fttr128.46")).unwrap();
        fs::write(root.join("bonding_masters"), "").unwrap();
        fs::write(root.join("fttr128.46/iflink"), "42\n").unwrap();
        assert!(verify_no_link_children(&root, "fttr128", "41").is_ok());
        assert!(verify_no_link_children(&root, "fttr128", "42").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_port_requires_matching_parent_protocol_and_vid() {
        let details = "vlan protocol 802.1ad id 2 <REORDER_HDR>";
        assert!(matching_vlan_details(
            details,
            "Device: fttr0\n",
            "fttr0",
            2
        ));
        assert!(!matching_vlan_details(
            details,
            "Device: eth0\n",
            "fttr0",
            2
        ));
        assert!(!matching_vlan_details(
            "vlan protocol 802.1Q id 2",
            "Device: fttr0",
            "fttr0",
            2
        ));
        assert!(!matching_vlan_details(details, "Device: fttr0", "fttr0", 3));
    }

    #[test]
    fn fast_proc_vlan_verification_checks_iface_vid_and_parent() {
        let proc_vlan = "\
fttr1  VID: 1\t REORDER_HDR: 1  dev->priv_flags: 1021
         total frames received        1200
Device: fttr0
INGRESS priority mappings: 0:0  1:0  2:0  3:0  4:0  5:0  6:0 7:0
";
        assert!(matching_proc_vlan(proc_vlan, "fttr1", "fttr0", 1));
        assert!(!matching_proc_vlan(proc_vlan, "fttr2", "fttr0", 1));
        assert!(!matching_proc_vlan(proc_vlan, "fttr1", "eth0", 1));
        assert!(!matching_proc_vlan(proc_vlan, "fttr1", "fttr0", 2));
    }
}
