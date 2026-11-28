use std::{
    io::{ErrorKind, Read},
    os::{
        fd::OwnedFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;

pub(super) fn run(program: &str, args: &[&str]) -> Result<String, String> {
    capture(program, args, Duration::from_secs(30))
}

fn capture(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let (mut stdout, writer) = UnixStream::pair().map_err(|_| "cannot create command output")?;
    stdout
        .set_nonblocking(true)
        .map_err(|_| "cannot configure command output")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| format!("cannot start {program}"))?;
    let result = (|| {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        let mut eof = false;
        let mut status = None;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("{program} timed out; rediscovery required"));
            }
            let mut progress = false;
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        if count > OUTPUT_LIMIT - bytes.len() {
                            return Err(format!(
                                "{program} output limit exceeded; rediscovery required"
                            ));
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        progress = true;
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => (),
                    Err(_) => return Err("command output failed".into()),
                }
            }
            if status.is_none() {
                status = child.try_wait().map_err(|_| "cannot inspect command")?;
            }
            if let Some(status) = status {
                if !status.success() {
                    return Err(format!("{program} failed; rediscovery required"));
                }
                if eof {
                    return String::from_utf8(bytes).map_err(|_| "invalid command output".into());
                }
            }
            if !progress {
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    })();
    drop(stdout);
    if result.is_err() {
        let process_group = i32::try_from(child.id())
            .map_err(|_| format!("cannot stop {program}: process id is out of range"))?;
        // SAFETY: child.id() identifies the process group created for this command.
        // A negative pid targets only that group; SIGKILL cannot affect the parent.
        let kill_result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        if kill_result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return match child.wait() {
                    Ok(_) => Err(format!("cannot stop {program}: {error}")),
                    Err(wait) => Err(format!(
                        "cannot stop {program}: {error}; cannot reap: {wait}"
                    )),
                };
            }
        }
        match child.kill() {
            Ok(()) => (),
            Err(error)
                if error.kind() == ErrorKind::InvalidInput
                    || error.raw_os_error() == Some(libc::ESRCH) => {}
            Err(error) => {
                return match child.wait() {
                    Ok(_) => Err(format!("cannot stop {program}: {error}")),
                    Err(wait) if wait.raw_os_error() == Some(libc::ECHILD) => {
                        Err(format!("cannot stop {program}: {error}"))
                    }
                    Err(wait) => Err(format!(
                        "cannot stop {program}: {error}; cannot reap: {wait}"
                    )),
                };
            }
        }
    }
    match child.wait() {
        Ok(_) => (),
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => (),
        Err(error) => return Err(format!("cannot reap {program}: {error}")),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_larger_than_socket_buffer_when_child_writes_before_exiting() {
        let script = "head -c 262144 /dev/zero | tr '\\000' x";
        let output = capture("sh", &["-c", script], Duration::from_secs(2)).unwrap();
        assert_eq!(output, "x".repeat(262144));
    }

    #[test]
    fn accepts_output_when_exactly_at_limit() {
        let output = capture(
            "head",
            &["-c", "1048576", "/dev/zero"],
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.len(), OUTPUT_LIMIT);
    }

    #[test]
    fn rejects_output_when_over_limit() {
        let error = capture(
            "head",
            &["-c", "1048577", "/dev/zero"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.contains("output limit exceeded"));
    }

    #[test]
    fn bounds_completion_when_descendant_keeps_stdout_after_direct_child_exits() {
        let started = Instant::now();
        let error = capture(
            "sh",
            &["-c", "sleep 1 & printf ready"],
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(800));
    }

    #[test]
    fn stops_descendant_before_its_delayed_side_effect_after_timeout() {
        let path = std::env::temp_dir().join(format!(
            "fttr-capture-{}-{}.side-effect",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let script = format!("sleep 0.3; echo ran > '{}' & exec sleep 10", path.display());
        let error = capture("sh", &["-c", &script], Duration::from_millis(100)).unwrap_err();
        std::thread::sleep(Duration::from_millis(400));
        assert!(error.contains("timed out"));
        assert!(!path.exists());
    }

    #[test]
    fn bounds_completion_when_child_continuously_writes() {
        let started = Instant::now();
        let error = capture("yes", &[], Duration::from_secs(2)).unwrap_err();
        assert!(error.contains("output limit exceeded"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reaps_direct_child_when_deadline_expires_after_stdout_closes() {
        let path = std::env::temp_dir().join(format!(
            "fttr-capture-{}-{}.pid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let error = capture(
            "sh",
            &[
                "-c",
                "echo $$ > \"$1\"; exec 1>&-; exec sleep 10",
                "capture",
                path.to_str().unwrap(),
            ],
            Duration::from_millis(100),
        )
        .unwrap_err();
        let pid = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(error.contains("timed out"));
        assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
    }

    #[test]
    fn rejects_output_when_child_exits_unsuccessfully() {
        let error = capture(
            "sh",
            &["-c", "printf partial; exit 7"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.contains("failed; rediscovery required"));
    }

    #[test]
    fn rejects_output_when_utf8_is_invalid() {
        let error = capture("sh", &["-c", "printf '\\377'"], Duration::from_secs(2)).unwrap_err();
        assert_eq!(error, "invalid command output");
    }
}
