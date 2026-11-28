use std::env;
use std::path::Path;
use std::process::exit;

use openwrt_fttr::ipc;
use openwrt_fttr::model::FttrSystemStatus;
use openwrt_fttr::platform::{context::PlatformContext, diagnostics::HardwareDiagnostics};
#[path = "fttrctl/hardware.rs"]
mod hardware_command;
#[cfg(feature = "device-hl202")]
#[path = "fttrctl/hl202.rs"]
mod hl202;
#[cfg(feature = "device-hl202")]
#[path = "fttrctl/mqtt_command.rs"]
mod mqtt_command;
#[path = "fttrctl/pon_stats.rs"]
mod pon_stats_command;
#[cfg(feature = "device-hl202")]
use mqtt_command::handle as handle_mqtt_command;

fn print_usage() {
    let usage =
        "Usage: fttrctl <command> [arguments]\n\n\
Commands:\n  \
  capabilities [--json] [--daemon]  Report compiled support (optionally from running fttrd)\n  \
  config check|reload  Validate or reload /etc/config/fttr desired state\n  \
  sync plan|apply [DEVICE|all] [--item SECTION] [--management] [--wait SECONDS]\n  \
  load-fpga [path]     Program FPGA bitstream (default: /lib/firmware/FTTR_TOP.sbit)\n  \
  bosa-init            Initialize BOSA optical transceiver registers\n  \
  bridge-setup         Reapply selected platform network topology through fttrd\n  \
  offload on|off|status [--json]  Control or query NPU / PPE hardware offload\n  \
  optical on|off|status|reload|shutdown|shutdown-offline [--json]\n  \
  status [--json|-j]   Display active FTTR optical link and DSA switch ports (table or JSON)\n  \
  mib ONU_ID [--summary|--json] [--tables]  Read structured OMCI inventory (default: JSON)\n  \
  fec ONU_ID [--json] [--create-pm]  Read downstream FEC PM; optionally create missing PM instance\n  \
  us-fec ONU_ID [on|off|status] [--json]  Read or toggle per-ONU upstream FEC (AN4475-G2)\n  \
  hardware DEVICE COMMAND  Exclusive offline register/memory diagnostics (JSON; hardware help)\n  \
  pon-stats ONU_ID [--samples N] [--interval-ms N] [--json]  Read-only AN4475-G2 PON counters / PHY\n  \
  fpga-read <addr>     Directly read 32-bit FPGA hardware register\n  \
  fpga-diag [ONU_ID|all] [--samples N] [--interval-ms N] [--json] [--offline]\n  \
  fpga-write <a> <v>   Directly write 32-bit FPGA hardware register\n  \
  bosa-diag [--json]   Read calibrated UX3326 optical diagnostics (Temp, Vcc, Bias, TX)\n  \
  bosa-read <addr>     Directly read 8-bit BOSA UX3326 optical register\n  \
  bosa-write <a> <v>   Directly write 8-bit BOSA UX3326 optical register\n  \
  hl202 <subcommand>   HL202 device reports, commands and accounts (hl202 help)\n  \
  exec [options] <mac> <command>  Execute, wait for combined output and exit status\n  \
  cmd [options] <mac> <command>   Dispatch without waiting for device execution\n  \
  broadcast [options] <command>  Broadcast dispatch, or --wait for per-device replies\n  \
  help                 Show this help message"
    ;
    for line in usage.lines() {
        let command = line.split_whitespace().next().unwrap_or("");
        if !cfg!(feature = "platform-h3c-hm2004-du")
            && matches!(
                command,
                "load-fpga"
                    | "bosa-init"
                    | "fpga-read"
                    | "fpga-write"
                    | "fpga-diag"
                    | "bosa-diag"
                    | "bosa-read"
                    | "bosa-write"
            )
        {
            continue;
        }
        if !cfg!(feature = "platform-fiberhome-an4475-g2") && command == "pon-stats" {
            continue;
        }
        if !cfg!(feature = "device-hl202")
            && matches!(command, "hl202" | "exec" | "cmd" | "broadcast")
        {
            continue;
        }
        eprintln!("{line}");
    }
}

fn status_ipc_error(error: &str) -> serde_json::Value {
    serde_json::json!({
        "status": "unavailable",
        "error": "fttrd status IPC failed",
        "detail": error,
    })
}

#[cfg(not(feature = "device-hl202"))]
fn handle_mqtt_command(_: &[String]) -> Result<i32, String> {
    Err("HL202 adapter not compiled; enable device-hl202".into())
}

fn hardware() -> HardwareDiagnostics {
    HardwareDiagnostics::detect().unwrap_or_else(|error| {
        eprintln!("Platform selection: {error}");
        exit(1)
    })
}

fn handle_offload(args: &[String]) {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("status");
    let is_json = args.iter().any(|a| a == "--json" || a == "-j");
    match sub {
        "status" => {
            let status = openwrt_fttr::npu::status();
            if is_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status).unwrap_or_default()
                );
            } else {
                println!("[*] NPU & Hardware Offload Status:");
                println!(
                    "    Engine attached: {} | Offload enabled: {}",
                    if status.engine_attached {
                        "YES (Ready)"
                    } else {
                        "NO"
                    },
                    if status.bridge_offload_enabled {
                        "YES"
                    } else {
                        "NO"
                    }
                );
                let active_ports = if status.active_ports.is_empty() {
                    "none".into()
                } else {
                    status.active_ports.join(", ")
                };
                if status.admission_model == Some(fttr_api::NpuAdmissionModel::DirectionOnly) {
                    let dir = if status.direction_mask_valid {
                        format!("0x{:x} ({})", status.direction_mask, status.direction_mask)
                    } else {
                        "unavailable".into()
                    };
                    println!(
                        "    Bridge: {} | Direction mask: {} | Eligible ports: {}",
                        status.bridge, dir, active_ports
                    );
                    println!(
                        "    PPE debug: {} | Hooks ready: {}",
                        if status.bridge_offload_debug {
                            "YES"
                        } else {
                            "NO"
                        },
                        if status.fw4_hooks_ready { "YES" } else { "NO" }
                    );
                } else {
                    println!(
                        "    Bridge: {} | Mask: 0x{:04x} ({}) | Active ports: {}",
                        status.bridge,
                        status.bridge_offload_mask,
                        status.bridge_offload_mask,
                        active_ports
                    );
                    println!(
                        "    Routed mask: 0x{:04x} | Driver debug: {} | Hooks ready: {}",
                        status.routed_offload_mask,
                        if status.bridge_offload_debug {
                            "YES"
                        } else {
                            "NO"
                        },
                        if status.fw4_hooks_ready { "YES" } else { "NO" }
                    );
                }
                println!(
                    "    FOE Entries: total {} | bound {} (IPv4: {}, IPv6: {}) | Offload traffic: {} pkts, {} bytes",
                    status.total_entries,
                    status.bound_entries,
                    status.bound_ipv4_entries,
                    status.bound_ipv6_entries,
                    status.total_packets,
                    status.total_bytes
                );
                if !status.bound_flows.is_empty() {
                    println!("    Active Bound Flows ({}):", status.bound_flows.len());
                    for flow in &status.bound_flows {
                        println!(
                            "      [{}] {} orig={} eth={} vlan={} pkts={} bytes={}",
                            flow.index,
                            flow.pkt_type,
                            flow.orig_tuple,
                            flow.eth,
                            flow.vlan,
                            flow.packets,
                            flow.bytes
                        );
                    }
                }
            }
        }
        "on" | "off" => {
            let enabled = sub == "on";
            #[cfg(all(
                feature = "npu",
                any(
                    feature = "platform-h3c-hm2004-du",
                    feature = "platform-fiberhome-an4475-g2"
                )
            ))]
            let bridge = args
                .iter()
                .position(|a| a == "--bridge")
                .map(|i| args.get(i + 1).map(String::as_str).unwrap_or(""));
            #[cfg(all(
                feature = "npu",
                any(
                    feature = "platform-h3c-hm2004-du",
                    feature = "platform-fiberhome-an4475-g2"
                )
            ))]
            let result = openwrt_fttr::npu::settings::set(enabled, bridge, None, None, None);
            #[cfg(not(all(
                feature = "npu",
                any(
                    feature = "platform-h3c-hm2004-du",
                    feature = "platform-fiberhome-an4475-g2"
                )
            )))]
            let result: std::io::Result<()> = Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "NPU adapter not compiled (enable Cargo feature npu)",
            ));
            match result {
                Ok(()) => {
                    if is_json {
                        println!(
                            "{}",
                            serde_json::json!({"status":"success","enabled":enabled})
                        );
                    } else {
                        println!(
                            "[+] NPU configuration saved; query status for admission readback."
                        );
                    }
                }
                Err(error) => {
                    if is_json {
                        println!(
                            "{}",
                            serde_json::json!({"status":"error","error":error.to_string()})
                        );
                    } else {
                        eprintln!("[-] {error}");
                    }
                    exit(1);
                }
            }
        }
        _ => {
            eprintln!("Usage: fttrctl offload [on|off|status] [--json]");
            exit(1);
        }
    }
}

fn wire_name(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn print_status_table(status: &FttrSystemStatus) {
    println!("==========================================================================================");
    println!(
        "                         FTTR Micro-OLT Subsystem Status                               "
    );
    println!("==========================================================================================");

    // 0. FPGA Core Status
    let fpga_badge = match status.fpga_status.as_str() {
        "ready" => "[READY / ACTIVE]",
        "programming" => "[PROGRAMMING / IN PROGRESS]",
        "failed" => "[FAILED / ERROR]",
        _ => "[UNKNOWN / NOT STARTED]",
    };
    println!(
        "[*] FPGA Micro-OLT Core:    {} (Status: {})",
        fpga_badge, status.fpga_status
    );
    if let Some(provisioning) = &status.provisioning {
        println!(
            "[*] Provisioning: {} / {} / revision {}",
            provisioning.enabled,
            wire_name(&provisioning.mode),
            provisioning.revision
        );
        if let Some(error) = &provisioning.error {
            println!("    Config error: {error}");
        }
    }
    println!();

    // 1. Optical Link
    let opt = &status.optical_transceiver;
    let tx_enable_str = match opt.tx_enabled {
        Some(true) => "ENABLED",
        Some(false) => "DISABLED",
        None => "UNKNOWN",
    };

    println!("[*] Optical Transceiver (BOSA {}):", opt.model);
    println!(
        "    TX enable/readback: {}  |  Wavelength: TX {}nm / RX {}nm  |  Rate: {:.3}G / {:.3}G",
        tx_enable_str,
        opt.wavelength_tx_nm,
        opt.wavelength_rx_nm,
        opt.phy_rate_downlink_gbps,
        opt.phy_rate_uplink_gbps
    );
    if opt.temperature_celsius.is_some()
        || opt.vcc_voltage.is_some()
        || opt.laser_bias_current_ma.is_some()
        || opt.tx_power_dbm.is_some()
    {
        let temp_str = opt
            .temperature_celsius
            .map(|t| format!("{t:.2} °C"))
            .unwrap_or_else(|| "N/A".into());
        let volt_str = opt
            .vcc_voltage
            .map(|v| format!("{v:.2} V"))
            .unwrap_or_else(|| "N/A".into());
        let bias_str = opt
            .laser_bias_current_ma
            .map(|b| format!("{b:.2} mA"))
            .unwrap_or_else(|| "N/A".into());
        let tx_str = opt
            .tx_power_dbm
            .map(|p| format!("{p:.2} dBm"))
            .unwrap_or_else(|| "N/A".into());
        println!(
            "    Diagnostics: Temp: {}  |  Voltage: {}  |  Bias: {}  |  TX: {}",
            temp_str, volt_str, bias_str, tx_str
        );
    } else {
        println!("    Optical diagnostics: unavailable (no verified hardware readings)");
    }
    println!();

    // 2. Switch Ports Table
    println!("------------------------------------------------------------------------------------------");
    println!(" Downstream ports");
    println!("----------------------------------------------------------------------------------------------------");
    println!(
        " {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<24}",
        "Port", "Interface", "Carrier", "Link Speed", "ONU-ID", "ONU Serial", "Link State"
    );
    println!("----------------------------------------------------------------------------------------------------");

    let mut active_count = 0;
    let mut in_prog_count = 0;
    for p in &status.switch_ports {
        let is_up = p.carrier;
        let sg_for_port = p
            .onu_id
            .and_then(|id| status.sub_gateways.iter().find(|sg| sg.onu_id == id));
        let speed = sg_for_port
            .and_then(|sg| match sg.upstream_phy_rate_gbps {
                Some(rate) if rate >= 2.0 => Some("2.5G/2.5G"),
                Some(_) => Some("2.5G/1.25G"),
                None => None,
            })
            .unwrap_or("Unavailable");
        let onu_id_str = p
            .onu_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".to_string());
        let onu_sn_str = p.onu_sn.as_deref().unwrap_or("-");
        let link_state = if !p.link_state.is_empty() {
            p.link_state.as_str()
        } else if is_up {
            "O5Operation (Online)"
        } else {
            "Offline"
        };

        if is_up {
            active_count += 1;
        } else if p.onu_id.is_some() || link_state != "Offline" {
            in_prog_count += 1;
        }

        println!(
            " {:<6} {:<10} {:<10} {:<12} {:<8} {:<16} {:<24}",
            p.port, p.interface, p.status, speed, onu_id_str, onu_sn_str, link_state
        );
    }
    println!("----------------------------------------------------------------------------------------------------");
    let total_ports = status.switch_ports.len();
    if in_prog_count > 0 {
        println!(
            " Downlink Ports: {} Carrier Up, {} Assigned Without Carrier / {} Total",
            active_count, in_prog_count, total_ports
        );
    } else {
        println!(" Active Downlink Ports: {} / {}", active_count, total_ports);
    }
    println!();

    // 3. Sub-Gateways Details (if any)
    if !status.sub_gateways.is_empty() {
        println!("------------------------------------------------------------------------------------------");
        println!(" Active Sub-Gateway Telemetry & Mesh Details");
        println!("------------------------------------------------------------------------------------------");
        for sg in &status.sub_gateways {
            println!(
                "[+] Sub-Gateway #{} [{}] - {} {}",
                sg.onu_id, sg.serial_number, sg.vendor, sg.model
            );
            println!(
                "    Switch Port:    {} (GEM: {}, State: {:?})",
                sg.data_path.interface,
                sg.data_path
                    .olt_channel_gem
                    .map(|gem| format!("0x{gem:03x}"))
                    .unwrap_or_else(|| "unknown".into()),
                sg.state
            );
            if sg.pon_mode.is_some() || sg.upstream_phy_rate_gbps.is_some() {
                let mode = sg.pon_mode.as_deref().unwrap_or("GPON");
                let rate = sg
                    .upstream_phy_rate_gbps
                    .map(|up| format!("2.488G / {up:.3}G"))
                    .unwrap_or_else(|| "Unavailable".into());
                println!("    PON Mode:       {} ({})", mode, rate);
            }
            if let Some(fec) = &sg.fec {
                let fmt_bool = |value: Option<bool>| match value {
                    Some(true) => "ENABLED",
                    Some(false) => "DISABLED",
                    None => "UNKNOWN",
                };
                println!(
                    "    FEC:            DS: {} | US: {}",
                    fmt_bool(fec.downstream_enabled),
                    fmt_bool(fec.upstream_enabled)
                );
            }
            println!("    Business:       {}", sg.business_status);
            if let Some(provisioning) = &sg.provisioning {
                println!(
                    "    CWMP:           {} | Sync: {} | Wi-Fi: {}",
                    sg.management
                        .as_ref()
                        .map(|m| wire_name(&m.state))
                        .unwrap_or_else(|| "unknown".into()),
                    wire_name(&provisioning.state),
                    wire_name(&sg.wifi_sync)
                );
                for item in sg.services.iter().chain(sg.ssids.iter()) {
                    println!(
                        "      {} [{}]: {} {}",
                        item.id,
                        item.service_type,
                        item.state,
                        item.reason.as_deref().unwrap_or("")
                    );
                }
            }
            println!("    Hardware:       {}", sg.hardware_version);
            println!("    Software:       {}", sg.firmware_version);
            if let Some(identity) = &sg.identity {
                let age = sg
                    .identity_age_seconds
                    .map(|seconds| format!("{seconds}s ago"))
                    .unwrap_or_else(|| "unknown age".into());
                println!(
                    "    Identity:       OMCI cache, {}, {age}",
                    if sg.identity_current {
                        "current registration"
                    } else {
                        "offline / cached"
                    }
                );
                println!(
                    "    OMCC version:   {}",
                    identity
                        .omcc_version
                        .map(|value| format!("0x{value:02x}"))
                        .unwrap_or_else(|| "Unavailable".into())
                );
                for error in &identity.errors {
                    println!("    Identity read:  {error}");
                }
            } else {
                println!("    Identity:       not collected yet");
            }
            if let Some(bits) = sg.equalization_delay_bits {
                println!(
                    "    Measured EqD:   {bits} bits (Ranging_Time sent; see protocol state above)"
                );
            }
            println!(
                "    Fiber Distance: {} | Optical RX: {} | Optical TX: {}",
                sg.fiber_distance_m
                    .map(|v| format!("{v:.1} m"))
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.optical_rx_power_dbm
                    .map(|v| format!("{v:.3} dBm"))
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.optical_tx_power_dbm
                    .map(|v| format!("{v:.3} dBm"))
                    .unwrap_or_else(|| "Unavailable".into())
            );
            println!(
                "    Wi-Fi Mesh:     2.4G Ch {} / 5G Ch {} ({})",
                sg.wifi_mesh
                    .channel_2g
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.wifi_mesh
                    .channel_5g
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
                sg.wifi_mesh
                    .bandwidth_5g
                    .as_deref()
                    .unwrap_or("Unavailable")
            );

            if let Some(clients) = &sg.connected_clients {
                println!("    Connected Mesh Clients:");
                for c in clients {
                    println!(
                        "      - {} | {} | {} ({} dBm) | {}/{} Mbps",
                        c.mac,
                        c.ip,
                        c.band,
                        c.rssi_dbm
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into()),
                        c.rx_rate_mbps
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into()),
                        c.tx_rate_mbps
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "Unavailable".into())
                    );
                }
            }
            println!();
        }
    }
    println!("==========================================================================================");
}

fn parse_optical_args(args: &[String]) -> Result<(&'static str, bool), String> {
    if args.len() < 3 {
        return Err(
            "Usage: fttrctl optical <on|off|status|reload|shutdown|shutdown-offline> [--json|-j]"
                .into(),
        );
    }
    if args[3..].iter().any(|arg| arg != "--json" && arg != "-j") {
        return Err("Unknown optical argument".into());
    }
    let sub = args[2].to_lowercase();
    let is_json = args.iter().any(|a| a == "--json" || a == "-j");
    match sub.as_str() {
        "on" => Ok(("OPTICAL ON", is_json)),
        "off" => Ok(("OPTICAL OFF", is_json)),
        "status" => Ok(("OPTICAL STATUS", is_json)),
        "reload" => Ok(("OPTICAL RELOAD", is_json)),
        "shutdown" => Ok(("OPTICAL SHUTDOWN", is_json)),
        "shutdown-offline" => Ok(("OFFLINE SHUTDOWN", is_json)),
        unknown => Err(format!("Unknown optical subcommand: {unknown}")),
    }
}

fn handle_optical_command(cmd: &str, is_json: bool) {
    let action = cmd
        .split_whitespace()
        .last()
        .unwrap_or("status")
        .to_ascii_lowercase();

    let result = if cmd == "OFFLINE SHUTDOWN" {
        let hardware = hardware();
        let start = std::time::Instant::now();
        loop {
            match openwrt_fttr::optical::claim_owner() {
                Ok(_owner) => {
                    let reset = hardware.set_optical_tx_enable(false);
                    let tx = hardware.get_optical_tx_enable().ok();
                    let reply = serde_json::json!({"schema_version":1,"status":if reset.is_ok() {"success"} else {"error"},"action":"optical_shutdown",
                        "desired_enabled":openwrt_fttr::optical::load_enabled().ok(),"tx_enabled":tx,"actor_state":if reset.is_ok() {"disabled"} else {"unknown"},
                        "hardware_ready":false,"discovery_active":false,"cleanup_pending":true,
                        "error":reset.err().map(|e| serde_json::json!({"code":format!("{:?}",e.kind()),"message":e.to_string()}))});
                    break Ok(reply.to_string());
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && start.elapsed().as_secs() < 6 =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(100))
                }
                Err(error) => break Err(error),
            }
        }
    } else {
        ipc::send_ipc_command(cmd)
    };
    let result = result.and_then(|response| {
        let value: serde_json::Value = serde_json::from_str(&response)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if !matches!(
            value.get("status").and_then(serde_json::Value::as_str),
            Some("success" | "error")
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid optical response status",
            ));
        }
        Ok(response)
    });
    match result {
        Ok(response) => {
            let daemon_error = match serde_json::from_str::<serde_json::Value>(&response) {
                Ok(value) => {
                    value.get("status").and_then(serde_json::Value::as_str) == Some("error")
                }
                Err(_) => true,
            };
            if is_json {
                println!("{response}");
            } else {
                println!("Optical {action}: {response}");
            }
            if daemon_error {
                exit(1);
            }
        }
        Err(error) => {
            if is_json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"status":"error","action":format!("optical_{action}"),
                    "desired_enabled":null,"tx_enabled":null,"actor_state":"unknown","hardware_ready":false,"discovery_active":false,"cleanup_pending":false,
                    "error":{"code":format!("{:?}",error.kind()),"message":format!("{error}; outcome unknown; query optical status")}})
                );
            } else {
                eprintln!("[-] Optical request failed: {error}");
            }
            exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        exit(1);
    }

    let _hardware_owner = if matches!(
        args[1].as_str(),
        "load-fpga" | "fpga-write" | "bosa-write" | "write-reg" | "write-bosa"
    ) {
        Some(
            openwrt_fttr::optical::claim_owner().unwrap_or_else(|error| {
                eprintln!("[-] Direct hardware write refused: {error}");
                exit(1);
            }),
        )
    } else {
        None
    };

    match args[1].as_str() {
        "capabilities" => {
            if args[2..]
                .iter()
                .any(|arg| arg != "--json" && arg != "-j" && arg != "--daemon")
            {
                eprintln!("Usage: fttrctl capabilities [--json|-j] [--daemon]");
                exit(1);
            }
            if args[2..].iter().any(|arg| arg == "--daemon") {
                match ipc::send_ipc_command("CAPABILITIES") {
                    Ok(reply) => print!("{reply}"),
                    Err(error) => {
                        eprintln!("Capabilities IPC failed: {error}");
                        exit(1);
                    }
                }
            } else {
                println!("{}", openwrt_fttr::capabilities::json());
            }
        }
        "acs" => match openwrt_fttr::acs_cli::run(&args[2..]) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("ACS: {e}");
                std::process::exit(1);
            }
        },
        "config" | "sync" => match openwrt_fttr::provisioning_cli::run(&args[1], &args[2..]) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("Provisioning: {e}");
                exit(1);
            }
        },
        "load-fpga" => {
            let path_str = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or("/lib/firmware/FTTR_TOP.sbit");
            let path = Path::new(path_str);
            if let Err(e) = hardware().load_bitstream(path) {
                eprintln!("[-] Error programming FPGA: {e}");
                exit(1);
            }
        }
        "bosa-init" => {
            handle_optical_command("OPTICAL REINITIALIZE", false);
        }
        "bridge-setup" => match ipc::send_ipc_command("BRIDGE SETUP") {
            Ok(reply) => {
                println!("{reply}");
                if !serde_json::from_str::<serde_json::Value>(&reply)
                    .is_ok_and(|v| v["status"] == "success")
                {
                    exit(1);
                }
            }
            Err(error) => {
                eprintln!("Bridge setup: {error}");
                exit(1);
            }
        },
        "optical" => match parse_optical_args(&args) {
            Ok((cmd, is_json)) => handle_optical_command(cmd, is_json),
            Err(err) => {
                eprintln!("[-] {err}");
                eprintln!("Usage: fttrctl optical <on|off|status|reload|shutdown|shutdown-offline> [--json|-j]");
                exit(1);
            }
        },
        "fpga-diag" => {
            if let Err(error) = hardware().fpga_diag(&args[2..]) {
                eprintln!("FPGA diagnostics: {error}");
                exit(1);
            }
        }
        "hardware" => {
            if let Err(error) = hardware_command::handle(&args[2..]) {
                eprintln!("[-] Hardware diagnostic: {error}");
                exit(1);
            }
        }
        "pon-stats" => {
            if let Err(error) = pon_stats_command::run(&args[2..]) {
                if error.kind() == std::io::ErrorKind::InvalidInput {
                    eprintln!("{error}");
                } else {
                    eprintln!("[-] PON stats diagnostic: {error}");
                }
                exit(1);
            }
        }
        "fpga-read" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl fpga-read <hex_or_dec_addr>");
                exit(1);
            }
            let addr_str = &args[2];
            let addr = if addr_str.starts_with("0x") || addr_str.starts_with("0X") {
                u32::from_str_radix(&addr_str[2..], 16).unwrap_or(0)
            } else {
                addr_str.parse::<u32>().unwrap_or(0)
            };
            match hardware().read_fpga_reg(addr) {
                Ok(val) => println!("FPGA Reg 0x{:08X} = 0x{:08X} ({})", addr, val, val),
                Err(e) => {
                    eprintln!("[-] Error reading FPGA reg 0x{:08X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "fpga-write" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl fpga-write <hex_or_dec_addr> <hex_or_dec_val>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            let val = if args[3].starts_with("0x") || args[3].starts_with("0X") {
                u32::from_str_radix(&args[3][2..], 16).unwrap_or(0)
            } else {
                args[3].parse::<u32>().unwrap_or(0)
            };
            match hardware().write_fpga_reg(addr, val) {
                Ok(()) => println!(
                    "[+] Successfully wrote FPGA reg 0x{:08X} = 0x{:08X}",
                    addr, val
                ),
                Err(e) => {
                    eprintln!("[-] Error writing FPGA reg 0x{:08X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "bosa-diag" => {
            let is_json = args.iter().any(|a| a == "--json" || a == "-j");
            // Only the daemon knows which coefficients were actually initialized.
            let opt: fttr_api::OpticalTransceiverStatus = match ipc::send_ipc_command("STATUS")
                .and_then(|reply| {
                    let value: serde_json::Value =
                        serde_json::from_str(&reply).map_err(std::io::Error::other)?;
                    serde_json::from_value(value.get("optical_transceiver").cloned().ok_or_else(
                        || std::io::Error::other("daemon did not return optical telemetry"),
                    )?)
                    .map_err(std::io::Error::other)
                }) {
                Ok(status) => status,
                Err(error) => {
                    eprintln!("[-] Owner optical diagnostics unavailable: {error}");
                    exit(1);
                }
            };
            if is_json {
                println!("{}", serde_json::to_string_pretty(&opt).unwrap_or_default());
            } else {
                println!("[*] Optical Transceiver (BOSA {}):", opt.model);
                println!(
                    "    Wavelength: TX {}nm / RX {}nm  |  Rate: {:.3}G / {:.3}G",
                    opt.wavelength_tx_nm,
                    opt.wavelength_rx_nm,
                    opt.phy_rate_downlink_gbps,
                    opt.phy_rate_uplink_gbps
                );
                let temp_str = opt
                    .temperature_celsius
                    .map(|t| format!("{t:.2} °C"))
                    .unwrap_or_else(|| "N/A".into());
                let volt_str = opt
                    .vcc_voltage
                    .map(|v| format!("{v:.2} V"))
                    .unwrap_or_else(|| "N/A".into());
                let bias_str = opt
                    .laser_bias_current_ma
                    .map(|b| format!("{b:.2} mA"))
                    .unwrap_or_else(|| "N/A".into());
                let tx_str = opt
                    .tx_power_dbm
                    .map(|p| format!("{p:.2} dBm"))
                    .unwrap_or_else(|| "N/A".into());
                println!(
                    "    Diagnostics: Temp: {}  |  Voltage: {}  |  Bias: {}  |  TX: {}",
                    temp_str, volt_str, bias_str, tx_str
                );
                println!("    Measurements Evidence:");
                for (name, ev) in &opt.measurements {
                    let raw_val = ev.raw.as_deref().unwrap_or("-");
                    let reason_val = ev.reason.as_deref().unwrap_or("none");
                    println!("      - {name:<22}: raw={raw_val:<6} unit={:<5} source=\"{}\" (reason: {reason_val})", ev.unit, ev.source);
                }
            }
        }
        "bosa-read" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl bosa-read <hex_or_dec_addr>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            match hardware().read_bosa_reg(addr) {
                Ok(val) => println!("BOSA Reg 0x{:02X} = 0x{:02X} ({})", addr, val, val),
                Err(e) => {
                    eprintln!("[-] Error reading BOSA reg 0x{:02X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "bosa-write" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl bosa-write <hex_or_dec_addr> <hex_or_dec_val>");
                exit(1);
            }
            let addr = if args[2].starts_with("0x") || args[2].starts_with("0X") {
                u32::from_str_radix(&args[2][2..], 16).unwrap_or(0)
            } else {
                args[2].parse::<u32>().unwrap_or(0)
            };
            let val = if args[3].starts_with("0x") || args[3].starts_with("0X") {
                u32::from_str_radix(&args[3][2..], 16).unwrap_or(0)
            } else {
                args[3].parse::<u32>().unwrap_or(0)
            };
            match hardware().write_bosa_reg(addr, val) {
                Ok(()) => println!(
                    "[+] Successfully wrote BOSA reg 0x{:02X} = 0x{:02X}",
                    addr, val
                ),
                Err(e) => {
                    eprintln!("[-] Error writing BOSA reg 0x{:02X}: {e}", addr);
                    exit(1);
                }
            }
        }
        "status" => {
            let is_json = args.iter().any(|a| a == "--json" || a == "-j");
            match ipc::send_ipc_command("STATUS") {
                Ok(resp) => {
                    if is_json {
                        println!("{resp}");
                    } else {
                        match serde_json::from_str::<FttrSystemStatus>(&resp) {
                            Ok(parsed) => print_status_table(&parsed),
                            Err(e) => {
                                eprintln!("[-] Failed to parse daemon status ({e}). Raw response:\n{resp}");
                            }
                        }
                    }
                }
                Err(error) => {
                    let fpga_st = PlatformContext::detect()
                        .ok()
                        .map(|platform| HardwareDiagnostics::new(platform).firmware_status())
                        .unwrap_or_default();
                    if fpga_st.eq_ignore_ascii_case("programming") {
                        if is_json {
                            println!("{{\"fpga_status\": \"programming\", \"message\": \"FPGA bitstream is currently programming in background, please wait...\"}}");
                        } else {
                            println!("[*] FPGA 位流正在内核后台烧录中（Micro-OLT 硬件正在初始化，约需数十秒），请稍候...");
                        }
                        return;
                    } else if fpga_st.eq_ignore_ascii_case("failed") {
                        if is_json {
                            println!("{{\"fpga_status\": \"failed\", \"error\": \"FPGA bitstream programming failed in kernel\"}}");
                        } else {
                            eprintln!(
                                "[!] 错误：内核 FPGA 位流烧录失败！请检查系统日志（dmesg）。"
                            );
                        }
                        return;
                    }

                    if is_json {
                        println!("{}", status_ipc_error(&error.to_string()));
                    } else {
                        eprintln!("[-] fttrd status unavailable: IPC request failed: {error}");
                    }
                    exit(1);
                }
            }
        }
        "fec" => {
            let Some((id, create_pm)) = parse_fec_args(&args[2..]) else {
                eprintln!("Usage: fttrctl fec ONU_ID [--json] [--create-pm]");
                exit(1);
            };
            if let Err(error) = openwrt_fttr::controller::fec::read(id, create_pm) {
                eprintln!("[-] FEC PM diagnostic failed: {error}");
                exit(1);
            }
        }
        "us-fec" => {
            let Some((id, enable, is_json)) = parse_us_fec_args(&args[2..]) else {
                eprintln!("Usage: fttrctl us-fec ONU_ID [on|off|status] [--json]");
                exit(1);
            };
            let payload = serde_json::json!({"onu_id": id, "enable": enable});
            let reply = match ipc::send_ipc_command(&format!("US_FEC {payload}")) {
                Ok(raw) => raw,
                Err(error) => {
                    if is_json {
                        println!("{}", status_ipc_error(&error.to_string()));
                    } else {
                        eprintln!("[-] US FEC request failed: {error}");
                    }
                    exit(1);
                }
            };
            let value: serde_json::Value = match serde_json::from_str(&reply) {
                Ok(v) => v,
                Err(error) => {
                    eprintln!("[-] Invalid US FEC response: {error}");
                    exit(1);
                }
            };
            if is_json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or(reply));
            } else if value.get("status").and_then(|v| v.as_str()) == Some("success") {
                let state = value
                    .get("upstream_fec_enabled")
                    .and_then(|v| v.as_bool())
                    .map(|b| if b { "ENABLED" } else { "DISABLED" })
                    .unwrap_or("UNKNOWN");
                if let Some(thd) = value.get("b_all_set_thd").and_then(|v| v.as_u64()) {
                    println!("[+] ONU {id} upstream FEC: {state} (B_ALL_SET_THD={thd})");
                } else {
                    println!("[+] ONU {id} upstream FEC: {state}");
                }
            } else {
                let err = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                eprintln!("[-] US FEC failed: {err}");
                exit(1);
            }
        }
        "mib" => {
            let id = args.get(2).and_then(|value| parse_onu_id(value));
            let summary = args.iter().any(|arg| arg == "--summary");
            let mut progress_id = None;
            let Some(id) = id else {
                eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables]");
                exit(1);
            };
            let mut options = args[3..].iter();
            while let Some(option) = options.next() {
                match option.as_str() {
                    "--summary" | "--json" | "--tables" => {}
                    "--progress-id" if progress_id.is_none() => {
                        progress_id = options.next().filter(|value| !value.is_empty()).cloned();
                        if progress_id.is_none() {
                            eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]");
                            exit(1);
                        }
                    }
                    _ => {
                        eprintln!("Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]");
                        exit(1);
                    }
                }
            }
            if summary && args.iter().any(|arg| arg == "--json") {
                eprintln!(
                    "Usage: fttrctl mib ONU_ID [--summary|--json] [--tables] [--progress-id ID]"
                );
                exit(1);
            }
            if let Err(error) = openwrt_fttr::controller::inventory::read_with_progress(
                id,
                summary,
                args.iter().any(|arg| arg == "--tables"),
                progress_id,
            ) {
                eprintln!("[-] MIB read failed: {error}");
                exit(1);
            }
        }
        "exec" | "cmd" | "broadcast" => match handle_mqtt_command(&args[1..]) {
            Ok(code) => exit(code),
            Err(error) => {
                eprintln!("[-] {error}");
                exit(1);
            }
        },
        "hl202" => {
            #[cfg(feature = "device-hl202")]
            match hl202::handle(&args[2..]) {
                Ok(code) => exit(code),
                Err(error) => {
                    eprintln!("[-] {error}");
                    exit(1);
                }
            }
            #[cfg(not(feature = "device-hl202"))]
            {
                eprintln!("HL202 adapter not compiled");
                exit(1);
            }
        }
        "read-reg" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-reg <addr>");
                exit(1);
            }
            let addr_str = &args[2];
            let addr = u32::from_str_radix(
                addr_str.trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex address");
            match hardware().read_fpga_reg(addr) {
                Ok(val) => println!("FPGA [0x{:08X}] = 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error reading FPGA register: {e}"),
            }
        }
        "write-reg" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-reg <addr> <val>");
                exit(1);
            }
            let addr = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex address");
            let val = u32::from_str_radix(
                args[3].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex value");
            match hardware().write_fpga_reg(addr, val) {
                Ok(_) => println!("FPGA [0x{:08X}] <= 0x{:08X}", addr, val),
                Err(e) => eprintln!("[-] Error writing FPGA register: {e}"),
            }
        }
        "read-bosa" => {
            if args.len() < 3 {
                eprintln!("Usage: fttrctl read-bosa <reg>");
                exit(1);
            }
            let reg = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex register");
            match hardware().read_bosa_reg(reg) {
                Ok(val) => println!("BOSA [0x{:02X}] = 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error reading BOSA register: {e}"),
            }
        }
        "write-bosa" => {
            if args.len() < 4 {
                eprintln!("Usage: fttrctl write-bosa <reg> <val>");
                exit(1);
            }
            let reg = u32::from_str_radix(
                args[2].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex register");
            let val = u32::from_str_radix(
                args[3].trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .expect("Invalid hex value");
            match hardware().write_bosa_reg(reg, val) {
                Ok(_) => println!("BOSA [0x{:02X}] <= 0x{:02X}", reg, val),
                Err(e) => eprintln!("[-] Error writing BOSA register: {e}"),
            }
        }
        "offload" => {
            handle_offload(&args);
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        unknown => {
            eprintln!("[-] Unknown command: {unknown}\n");
            print_usage();
            exit(1);
        }
    }
}

fn parse_onu_id(value: &str) -> Option<u8> {
    value
        .parse::<u8>()
        .ok()
        .filter(|id| openwrt_fttr::pon_engine::onu_id::is_valid(u16::from(*id)))
}

fn parse_fec_args(args: &[String]) -> Option<(u8, bool)> {
    let id = parse_onu_id(args.first()?)?;
    args[1..]
        .iter()
        .all(|arg| matches!(arg.as_str(), "--json" | "--create-pm"))
        .then(|| (id, args.iter().any(|arg| arg == "--create-pm")))
}

fn parse_us_fec_args(args: &[String]) -> Option<(u8, Option<bool>, bool)> {
    let id = parse_onu_id(args.first()?)?;
    let mut enable = None;
    let mut is_json = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--json" | "-j" => is_json = true,
            "on" if enable.is_none() => enable = Some(true),
            "off" if enable.is_none() => enable = Some(false),
            "status" if enable.is_none() => {}
            _ => return None,
        }
    }
    Some((id, enable, is_json))
}

#[cfg(test)]
mod tests {
    use super::{parse_fec_args, parse_optical_args, parse_us_fec_args, status_ipc_error};

    #[test]
    fn fec_accepts_protocol_ids_including_sparse_platform_ids_but_not_reserved_addresses() {
        use openwrt_fttr::pon_engine::onu_id::{MAX_ONU_ID, MIN_ONU_ID};
        for id in [MIN_ONU_ID, 15, 16, 72, MAX_ONU_ID] {
            assert_eq!(
                parse_fec_args(&[id.to_string(), "--create-pm".into(), "--json".into()]),
                Some((id, true))
            );
            assert_eq!(parse_fec_args(&[id.to_string()]), Some((id, false)));
        }
        for value in ["254", "255", "256", "-1", "bad"] {
            assert!(parse_fec_args(&[value.into()]).is_none());
        }
        assert!(parse_fec_args(&[]).is_none());
        assert!(parse_fec_args(&["72".into(), "--invalid".into()]).is_none());
    }

    #[test]
    fn ipc_failure_is_an_explicit_unavailable_result_not_device_status() {
        let response = status_ipc_error("Connection refused");
        assert_eq!(response["status"], "unavailable");
        assert_eq!(response["error"], "fttrd status IPC failed");
        assert_eq!(response["detail"], "Connection refused");
        assert!(response.get("devices").is_none());
        assert!(response.get("sub_gateways").is_none());
    }

    #[test]
    fn parse_optical_args_valid_and_invalid() {
        let to_args = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Insufficient arguments
        assert!(parse_optical_args(&to_args(&["fttrctl", "optical"])).is_err());

        // Valid commands
        assert_eq!(
            parse_optical_args(&to_args(&["fttrctl", "optical", "on"])).unwrap(),
            ("OPTICAL ON", false)
        );
        assert_eq!(
            parse_optical_args(&to_args(&["fttrctl", "optical", "off", "--json"])).unwrap(),
            ("OPTICAL OFF", true)
        );
        assert_eq!(
            parse_optical_args(&to_args(&["fttrctl", "optical", "status", "-j"])).unwrap(),
            ("OPTICAL STATUS", true)
        );

        // Invalid subcommand
        assert!(parse_optical_args(&to_args(&["fttrctl", "optical", "reboot"])).is_err());
    }

    #[test]
    fn parse_us_fec_args_valid_and_invalid() {
        let to_args = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_us_fec_args(&to_args(&["0"])), Some((0, None, false)));
        assert_eq!(
            parse_us_fec_args(&to_args(&["0", "on", "--json"])),
            Some((0, Some(true), true))
        );
        assert_eq!(
            parse_us_fec_args(&to_args(&["15", "off"])),
            Some((15, Some(false), false))
        );
        assert_eq!(
            parse_us_fec_args(&to_args(&["1", "status", "-j"])),
            Some((1, None, true))
        );
        assert!(parse_us_fec_args(&to_args(&["0", "on", "off"])).is_none());
        assert!(parse_us_fec_args(&to_args(&["255", "on"])).is_none());
    }
}
