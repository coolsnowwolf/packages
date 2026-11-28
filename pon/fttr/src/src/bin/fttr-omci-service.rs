//! Optional diagnostic entry; production configuration runs inside fttrd.
fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let id = args
        .get(1)
        .ok_or_else(|| {
            std::io::Error::other("usage: fttr-omci-service ONU_ID [SECONDS] [OPTIONS]")
        })?
        .parse()
        .map_err(std::io::Error::other)?;
    let seconds = args
        .get(2)
        .map(|value| value.parse().map_err(std::io::Error::other))
        .transpose()?;
    openwrt_fttr::controller::profile::configure(
        id,
        seconds,
        args.iter().any(|s| s == "--transparent-defaults"),
        args.iter().any(|s| s == "--wait-o5"),
        args.iter().any(|s| s == "--restore-initial-alloc"),
        None,
    )
}
