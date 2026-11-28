use super::diagnostics::{self as fpga_diag, Request, Snapshot};
use std::{io, time::Duration};

const USAGE: &str = "fttrctl fpga-diag [ONU_ID|all] [--samples 1..120] [--interval-ms 100..10000] [--json] [--offline]";
struct Options {
    request: Request,
    samples: u32,
    interval_ms: u64,
    json: bool,
    offline: bool,
}
fn parse(args: &[String]) -> io::Result<Options> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, USAGE);
    let mut options = Options {
        request: Request::default(),
        samples: 1,
        interval_ms: 1000,
        json: false,
        offline: false,
    };
    let mut target = false;
    let mut samples = false;
    let mut interval = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" | "-j" if !options.json => options.json = true,
            "--offline" if !options.offline => options.offline = true,
            "--samples" if !samples => {
                options.samples = args
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                samples = true;
            }
            "--interval-ms" if !interval => {
                options.interval_ms = args
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                interval = true;
            }
            "all" if !target => target = true,
            value if !target && !value.starts_with('-') => {
                options.request.onu_id = Some(value.parse().map_err(|_| invalid())?);
                target = true;
            }
            _ => return Err(invalid()),
        }
    }
    options.request.slots()?;
    if !(1..=120).contains(&options.samples)
        || !(100..=10000).contains(&options.interval_ms)
        || u64::from(options.samples - 1) * options.interval_ms > 60000
    {
        return Err(invalid());
    }
    Ok(options)
}

pub fn run(args: &[String]) -> io::Result<()> {
    if args.len() == 1 && matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let options = parse(args)?;
    let mut previous = None;
    let window = std::time::Instant::now();
    let mut next = window;
    for sample in 0..options.samples {
        if sample != 0 {
            std::thread::sleep(next.saturating_duration_since(std::time::Instant::now()));
            if window.elapsed() > Duration::from_secs(60) {
                break;
            }
        }
        next = std::time::Instant::now() + Duration::from_millis(options.interval_ms);
        let snapshot = if options.offline {
            fpga_diag::snapshot(&options.request, super::fmcs::read_fpga_reg)?
        } else {
            let command = format!("FPGA_DIAG {}", serde_json::to_string(&options.request)?);
            let response = crate::ipc::send_ipc_command(&command)?;
            let value: serde_json::Value = serde_json::from_str(&response)?;
            if let Some(error) = value.get("error") {
                return Err(io::Error::other(format!("FPGA diagnostic IPC: {error}")));
            }
            serde_json::from_value::<Snapshot>(value)?
        };
        let delta = previous
            .as_ref()
            .map(|old| fpga_diag::delta(old, &snapshot));
        if options.json {
            println!(
                "{}",
                serde_json::json!({"snapshot": snapshot, "delta": delta})
            );
        } else {
            println!(
                "Sample {} at {} ms; read {} us; main bandwidth enabled={} (0x{:08x})",
                sample + 1,
                snapshot.started_unix_ms,
                snapshot.read_duration_us,
                snapshot.main_bandwidth_enabled,
                snapshot.main_bandwidth_raw
            );
            for onu in &snapshot.onus {
                println!("ONU {:2} gen={:?} LoB={:5} BIP={:5} management: bit28={} bw={} weight={} control=0x{:08x}; business: bit28={} bw={} weight={} control=0x{:08x}",
                    onu.slot, onu.generation, onu.lob, onu.bip, onu.management.enabled_bit28, onu.management.bandwidth, onu.management.weight, onu.management.control_raw,
                    onu.business.enabled_bit28, onu.business.bandwidth, onu.business.weight, onu.business.control_raw);
            }
            for (name, value) in &snapshot.counters {
                println!("  {name}: {value}");
            }
            if let Some(delta) = delta {
                println!("Modulo deltas: {}", serde_json::to_string(&delta)?);
            }
            if sample == 0 {
                println!(
                    "Source: moltdbg SHA256 {}\n{}",
                    fpga_diag::SOURCE_SHA256,
                    fpga_diag::LIMITATIONS
                );
            }
        }
        previous = Some(snapshot);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    fn arguments_are_bounded_and_unambiguous_before_hardware_access() {
        assert!(parse(&args(&[
            "15",
            "--samples",
            "120",
            "--interval-ms",
            "100",
            "--json"
        ]))
        .is_ok());
        for values in [
            vec!["16"],
            vec!["0", "all"],
            vec!["--samples", "0"],
            vec!["--samples", "121"],
            vec!["--samples", "120", "--interval-ms", "10000"],
            vec!["--interval-ms", "99"],
            vec!["--samples"],
            vec!["--json", "--json"],
            vec!["--write"],
        ] {
            assert!(parse(&args(&values)).is_err(), "{values:?}");
        }
    }
}
