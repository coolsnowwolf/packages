fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let id = args
        .get(1)
        .ok_or_else(|| std::io::Error::other("usage: fttr-omci-mib ONU_ID [--summary] [--tables]"))?
        .parse()
        .map_err(std::io::Error::other)?;
    if args[2..]
        .iter()
        .any(|arg| !matches!(arg.as_str(), "--summary" | "--tables"))
    {
        return Err(std::io::Error::other(
            "usage: fttr-omci-mib ONU_ID [--summary] [--tables]",
        ));
    }
    openwrt_fttr::controller::inventory::read(
        id,
        args.iter().any(|arg| arg == "--summary"),
        args.iter().any(|arg| arg == "--tables"),
    )
}
