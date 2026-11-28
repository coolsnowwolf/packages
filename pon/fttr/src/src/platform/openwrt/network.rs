//! Linux network setup from a selected platform's topology.
use crate::platform::resources::Resources;
use std::{io, process::Command};

pub fn setup_bridge(resources: &Resources, bridge: &str) -> io::Result<()> {
    for interface in resources.management_interfaces {
        link(&["set", interface, "up"])?;
    }
    for interface in resources.downstream_interfaces {
        link(&["set", interface, "up"])?;
        link(&["set", interface, "master", bridge])?;
    }
    Ok(())
}
fn link(args: &[&str]) -> io::Result<()> {
    let status = Command::new("ip").arg("link").args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ip link {}: {status}",
            args.join(" ")
        )))
    }
}
