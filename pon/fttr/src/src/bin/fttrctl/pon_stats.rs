//! Read-only AN4475-G2 PON counter sampling through the running fttrd.
use fttr_api::{delta, PonStatsDelta, PonStatsRequest, PonStatsSnapshot};
use openwrt_fttr::ipc;
use std::{io, thread, time::Duration};

const USAGE: &str = "Usage: fttrctl pon-stats ONU_ID [--samples N] [--interval-ms N] [--json]
  Read-only FSL PON counters / PHY registers from the live fttrd session.
  --samples 1..120, --interval-ms 100..10000, total window <= 60s.
  --serial / --generation override the identity read from fttrd STATUS.
  Counters are raw hardware observations, not ONU FEC PM or link readiness.
  Modulo deltas are withheld when identity or clock continuity is broken.";

const LIMITATIONS: &str = "Non-atomic register reads; modulo deltas cannot distinguish resets from multiple wraps. PHY configuration is read verbatim. No state is written, reset or cleared.";

struct Options {
    request: PonStatsRequest,
    samples: u32,
    interval_ms: u64,
    json: bool,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn parse(args: &[String]) -> io::Result<Options> {
    let onu_id = args
        .first()
        .ok_or_else(|| invalid(USAGE))?
        .parse::<u8>()
        .map_err(|_| invalid("ONU_ID must be 0..255"))?;
    let mut serial = None;
    let mut generation = None;
    let mut samples = 1u32;
    let mut interval_ms = 1000u64;
    let mut json = false;
    let mut seen_samples = false;
    let mut seen_interval = false;
    let mut iter = args[1..].iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" | "-j" if !json => json = true,
            "--samples" if !seen_samples => {
                samples = iter
                    .next()
                    .ok_or_else(|| invalid(USAGE))?
                    .parse()
                    .map_err(|_| invalid("invalid --samples"))?;
                seen_samples = true;
            }
            "--interval-ms" if !seen_interval => {
                interval_ms = iter
                    .next()
                    .ok_or_else(|| invalid(USAGE))?
                    .parse()
                    .map_err(|_| invalid("invalid --interval-ms"))?;
                seen_interval = true;
            }
            "--serial" => serial = Some(iter.next().ok_or_else(|| invalid(USAGE))?.clone()),
            "--generation" => {
                generation = Some(
                    iter.next()
                        .ok_or_else(|| invalid(USAGE))?
                        .parse::<u64>()
                        .map_err(|_| invalid("invalid --generation"))?,
                )
            }
            _ => return Err(invalid(USAGE)),
        }
    }
    if !(1..=120).contains(&samples)
        || !(100..=10000).contains(&interval_ms)
        || u64::from(samples - 1) * interval_ms > 60_000
    {
        return Err(invalid("sampling window must be within 60 seconds"));
    }
    Ok(Options {
        request: PonStatsRequest {
            onu_id,
            serial: serial.unwrap_or_default(),
            generation: generation.unwrap_or_default(),
        },
        samples,
        interval_ms,
        json,
    })
}

fn identity(options: &mut Options) -> io::Result<()> {
    if !options.request.serial.is_empty() && options.request.generation != 0 {
        return Ok(());
    }
    let reply = ipc::send_ipc_command("STATUS")?;
    let status: fttr_api::StatusReply =
        serde_json::from_str(&reply).map_err(|e| invalid(&format!("bad STATUS reply: {e}")))?;
    let onu = status
        .sub_gateways
        .iter()
        .find(|onu| onu.onu_id == options.request.onu_id)
        .ok_or_else(|| invalid("ONU not found in fttrd status"))?;
    if options.request.serial.is_empty() {
        options.request.serial = onu.serial_number.clone();
    }
    if options.request.generation == 0 {
        options.request.generation = onu.business_status.generation;
    }
    Ok(())
}

fn sample(request: &PonStatsRequest) -> io::Result<PonStatsSnapshot> {
    let line = format!("PON_STATS {}", serde_json::to_string(request)?);
    let reply = ipc::send_ipc_command(&line)?;
    let value: serde_json::Value = serde_json::from_str(&reply)
        .map_err(|e| invalid(&format!("invalid PON_STATS reply: {e}")))?;
    if let Some(error) = value.get("error") {
        return Err(invalid(&format!("PON_STATS: {error}")));
    }
    serde_json::from_value(
        value
            .get("snapshot")
            .cloned()
            .ok_or_else(|| invalid("PON_STATS reply has no snapshot"))?,
    )
    .map_err(|e| invalid(&format!("invalid snapshot: {e}")))
}

fn print_human(onu_id: u8, snapshot: &PonStatsSnapshot, delta: Option<&PonStatsDelta>) {
    let counter = |d: Option<&PonStatsDelta>, key: &str| -> String {
        d.and_then(|d| d.counters_modulo_4294967296.as_ref())
            .and_then(|m| m.get(key))
            .map(|v| format!("+{v}"))
            .unwrap_or_else(|| "-".into())
    };
    println!(
        "PON stats ONU {} index={} schema={} (Unix ms {})",
        onu_id, snapshot.onu_index, snapshot.schema_version, snapshot.started_unix_ms
    );
    if let Some(d) = delta {
        println!(
            "Delta: {} ms, continuity={}",
            d.elapsed_ms, d.continuity_verified
        );
    }
    println!(
        "  upstream burst: det_ok={} ({}) det_fail={} ({}) bip_err={} ({})",
        snapshot.us_gtc.det_ok_normal,
        counter(delta, "us_gtc.det_ok_normal"),
        snapshot.us_gtc.det_fail_normal,
        counter(delta, "us_gtc.det_fail_normal"),
        snapshot.us_gtc.bip_err,
        counter(delta, "us_gtc.bip_err")
    );
    println!(
        "  GEM eth frames: ds={} ({}) us={} ({}) hec_uncorrect={} ({})",
        snapshot.ds_gem.eth_lf1,
        counter(delta, "ds_gem.eth_lf1"),
        snapshot.us_gem.eth_lf1,
        counter(delta, "us_gem.eth_lf1"),
        snapshot.us_gem.hec_uncorrect,
        counter(delta, "us_gem.hec_uncorrect")
    );
    println!("  {LIMITATIONS}");
}

pub fn run(args: &[String]) -> io::Result<()> {
    if args.len() == 1 && matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut options = parse(args)?;
    identity(&mut options)?;
    let mut previous: Option<PonStatsSnapshot> = None;
    for index in 0..options.samples {
        if index != 0 {
            thread::sleep(Duration::from_millis(options.interval_ms));
        }
        let snapshot = sample(&options.request)?;
        let delta = previous.as_ref().map(|old| delta(old, &snapshot));
        if options.json {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": snapshot.schema_version,
                    "limitations": LIMITATIONS,
                    "snapshot": snapshot,
                    "delta": delta,
                })
            );
        } else {
            print_human(options.request.onu_id, &snapshot, delta.as_ref());
        }
        previous = Some(snapshot);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn parse_rejects_invalid_arguments_before_any_ipc() {
        for values in [
            vec![],
            vec!["1", "--samples", "0"],
            vec!["1", "--samples", "121"],
            vec!["1", "--interval-ms", "99"],
            vec!["1", "--interval-ms", "10001"],
            vec!["1", "--samples", "62", "--interval-ms", "1000"],
            vec!["1", "--bogus"],
            vec!["abc"],
        ] {
            assert!(parse(&args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn parse_accepts_valid_options_and_explicit_identity() {
        let opts = parse(&args(&[
            "0",
            "--samples",
            "3",
            "--interval-ms",
            "250",
            "--json",
            "--serial",
            "F0AB000000000000",
            "--generation",
            "7",
        ]))
        .unwrap();
        assert_eq!(opts.samples, 3);
        assert_eq!(opts.interval_ms, 250);
        assert!(opts.json);
        assert_eq!(opts.request.generation, 7);
    }
}
