//! Persisted offload settings shared by the CLI and native controller.
use fttr_api::NpuAdmissionModel;
use std::{io, path::Path};

pub fn set(
    enabled: bool,
    bridge: Option<&str>,
    wan_upstream: Option<bool>,
    wan_downstream: Option<bool>,
    onu_bridge: Option<bool>,
) -> io::Result<()> {
    let Some(model) = super::active_admission_model() else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "selected platform has no NPU implementation",
        ));
    };
    if let Some(bridge) = bridge {
        if bridge.is_empty()
            || bridge.len() > 15
            || !bridge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(io::Error::other("Invalid bridge interface"));
        }
    }
    let delta = Path::new("/tmp/.uci/fttr");
    if delta.exists() && std::fs::metadata(delta)?.len() > 0 {
        return Err(io::Error::other(
            "FTTR has pending UCI changes; apply or revert them first",
        ));
    }
    let mut uci = openwrt_uci::Uci::new().map_err(io::Error::other)?;
    uci.ensure_section("fttr", "offload", "offload")
        .map_err(io::Error::other)?;
    uci.set(
        "fttr",
        "offload",
        "enabled",
        if enabled { "1" } else { "0" },
    )
    .map_err(io::Error::other)?;
    if let Some(bridge) = bridge {
        uci.set("fttr", "offload", "bridge", bridge)
            .map_err(io::Error::other)?;
    }
    for (option, value) in [
        ("wan_upstream", wan_upstream),
        ("wan_downstream", wan_downstream),
        ("onu_bridge", onu_bridge),
    ] {
        if let Some(value) = value {
            uci.set("fttr", "offload", option, if value { "1" } else { "0" })
                .map_err(io::Error::other)?;
        }
    }
    uci.commit("fttr").map_err(io::Error::other)?;
    if !enabled {
        match model {
            NpuAdmissionModel::PortMask => {
                withdraw(
                    Path::new(super::reconciler::MASK),
                    Path::new(super::reconciler::DIRECTIONS),
                )?;
            }
            NpuAdmissionModel::DirectionOnly => {
                withdraw_direction_only(Path::new(super::reconciler::DIRECTIONS))?;
            }
        }
    }
    Ok(())
}

fn withdraw(mask: &Path, directions: &Path) -> io::Result<()> {
    let mut admission = super::reconciler::Admission::new_with_directions(mask, directions);
    admission.withdraw()?;
    if admission.current()? != 0 || admission.current_mode(true)? != 0 {
        return Err(io::Error::other(
            "Saved, but driver withdrawal is not confirmed",
        ));
    }
    Ok(())
}

fn withdraw_direction_only(directions: &Path) -> io::Result<()> {
    let mut admission = super::reconciler::Admission::new_direction_only(directions);
    admission.withdraw()?;
    if admission.current_directions()? != 0 {
        return Err(io::Error::other(
            "Saved, but driver withdrawal is not confirmed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_wan_and_bridge_modes_are_permitted() {
        for (upstream, downstream) in [(true, false), (false, true), (true, true)] {
            let result = set(
                true,
                Some("br-lan"),
                Some(upstream),
                Some(downstream),
                Some(true),
            );
            if let Err(error) = result {
                assert!(!error.to_string().contains("exclusive port modes"));
            }
        }
    }

    #[test]
    fn disabling_withdraws_routed_as_well_as_bridge_admission() {
        let dir = std::env::temp_dir().join(format!("fttr-npu-disable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bridge = dir.join("bridge_offload_mask");
        let routed = dir.join("routed_offload_mask");
        let directions = dir.join("fttr_offload_directions");
        std::fs::write(&bridge, "0\n").unwrap();
        std::fs::write(&routed, "11\n").unwrap();
        std::fs::write(&directions, "7\n").unwrap();
        withdraw(&bridge, &directions).unwrap();
        assert_eq!(std::fs::read_to_string(&routed).unwrap().trim(), "0");
        assert_eq!(std::fs::read_to_string(&directions).unwrap().trim(), "0");
        std::fs::write(&bridge, "11\n").unwrap();
        withdraw(&bridge, &directions).unwrap();
        assert_eq!(std::fs::read_to_string(&bridge).unwrap().trim(), "0");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabling_withdraws_direction_only_admission() {
        let dir =
            std::env::temp_dir().join(format!("fttr-npu-disable-dironly-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let directions = dir.join("fttr_offload_directions");
        std::fs::write(&directions, "7\n").unwrap();
        withdraw_direction_only(&directions).unwrap();
        assert_eq!(std::fs::read_to_string(&directions).unwrap().trim(), "0");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
