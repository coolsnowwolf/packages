// Only the daemon selects this allocator; CLI and standalone tools keep System.
#[global_allocator]
static ALLOCATOR: openwrt_fttr::memory::Allocator = openwrt_fttr::memory::Allocator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--capabilities"] {
        println!("{}", openwrt_fttr::capabilities::json());
        return;
    }
    if args == ["--platform-info"] {
        println!("{}", openwrt_fttr::platform::runtime::report_json());
        return;
    }
    if args == ["--platform-check"] {
        if openwrt_fttr::platform::context::PlatformContext::detect().is_err() {
            std::process::exit(1);
        }
        return;
    }
    if !args.is_empty() {
        eprintln!("Usage: fttrd [--capabilities|--platform-info|--platform-check]");
        std::process::exit(1);
    }
    let platform =
        openwrt_fttr::platform::context::PlatformContext::detect().unwrap_or_else(|error| {
            eprintln!("[-] Platform selection failed closed: {error}");
            std::process::exit(1);
        });
    if let Err(error) = openwrt_fttr::startup_config::initialize() {
        eprintln!("[-] Could not load startup configuration: {error}");
        std::process::exit(1);
    }
    if let Err(error) = openwrt_fttr::capabilities::publish_menu() {
        eprintln!("[-] Could not refresh LuCI capability markers: {error}");
    }
    if let Err(error) = openwrt_fttr::platform::runtime::run_selected(platform) {
        eprintln!("[-] Platform runtime failed: {error}");
        std::process::exit(1);
    }
}
