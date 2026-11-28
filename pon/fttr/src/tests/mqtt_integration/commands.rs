use super::*;
use openwrt_fttr::devices::hl202::command;

#[test]
fn mqtt_commands_without_daemon_authority_never_use_the_broker_directly() {
    // A live broker is insufficient authority to execute a script. Transport
    // framing remains covered by real socket peers in command_transport tests.
    let mut addrs = addresses();
    addrs[0] = "127.0.0.1:1883".parse().unwrap();
    let (_fixture, rx) = child(&addrs);
    assert!(ready(&rx));
    for args in [
        vec!["exec", "--json", "020000000071", "fixture-only"],
        vec!["cmd", "020000000071", "fixture-only"],
        vec!["broadcast", "--targets", "020000000071", "fixture-only"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fttrctl"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let error =
        command::execute("020000000071", "fixture-only", Duration::from_secs(1)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("fttrd authority"));
    assert_eq!(
        command::dispatch("all", "fixture-only").unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[test]
fn mqtt_commands_respect_a_transaction_gate_held_by_another_process() {
    // The gate is process-wide rather than a mutex private to one client.
    let mut owner = Fixture(
        Command::new("flock")
            .args([
                "-n",
                "/tmp/fttr-command-transaction-020000000072.lock",
                "sh",
                "-c",
                "printf ready; read ignored",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    use std::io::Read;
    let mut ready = [0; 5];
    owner
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready");
    assert_eq!(
        command::execute("020000000072", "fixture-only", Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    // Another target reaches the daemon-authority check instead of that gate.
    assert_eq!(
        command::execute("020000000073", "fixture-only", Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    drop(owner.0.stdin.take());
    owner.0.wait().unwrap();
}
