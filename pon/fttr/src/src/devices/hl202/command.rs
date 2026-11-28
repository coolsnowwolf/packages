//! Shell-over-MQTT command transport. Separate connections from fttrd telemetry.
#[cfg(feature = "mqtt")]
use crate::mqtt::session::CommandSession;
pub use fttr_api::{HlCommandResult as CommandResult, HlCommandStatus as CommandStatus};
#[cfg(any(feature = "mqtt", test))]
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(any(feature = "mqtt", test))]
use std::os::fd::AsRawFd;
use std::time::Duration;
#[cfg(any(feature = "mqtt", test))]
use std::time::{SystemTime, UNIX_EPOCH};

pub fn normalize_mac(value: &str) -> io::Result<String> {
    let mac = if value.len() == 17
        && value.split(':').count() == 6
        && value.split(':').all(|part| part.len() == 2)
    {
        value.replace(':', "")
    } else {
        value.to_string()
    };
    if mac.len() != 12 || !mac.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target must be the 12-digit br0 MAC (e.g. 1A2B3C4DA111), not the optical SN",
        ));
    }
    Ok(mac.to_ascii_uppercase())
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn request_id() -> String {
    format!(
        "{:x}_{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(any(feature = "mqtt", test))]
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn framed_script(command: &str, id: &str) -> String {
    format!("printf '__FTTR_BEGIN_{id}__\\n'; sh -c {} 2>&1; fttr_rc=$?; printf '\\n__FTTR_END_{id}__=%s\\n' \"$fttr_rc\"",
        shell_quote(command))
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn parse_reply(payload: &[u8], id: &str) -> Option<(String, i32, bool)> {
    let text = String::from_utf8_lossy(payload);
    let body = text.strip_prefix(&format!("__FTTR_BEGIN_{id}__\n"))?;
    let (output, code) = body.rsplit_once(&format!("\n__FTTR_END_{id}__="))?;
    let code = code.strip_suffix('\n')?.parse::<i32>().ok()?;
    let mut end = output.len().min(1024 * 1024);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    Some((output[..end].to_string(), code, end < output.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn mqtt_command_output_limit_preserves_utf8_and_exit_status() {
        // Given output crossing the byte limit inside a UTF-8 character.
        let output = format!("{}猫", "a".repeat(1024 * 1024 - 1));
        let payload = format!("__FTTR_BEGIN_limit__\n{output}\n__FTTR_END_limit__=7\n");
        // When parsing the matching response, then retain valid bounded text and exit proof.
        let (text, code, truncated) = parse_reply(payload.as_bytes(), "limit").unwrap();
        assert_eq!(text.len(), 1024 * 1024 - 1);
        assert_eq!(code, 7);
        assert!(truncated);
    }

    #[test]
    fn mqtt_command_output_limit_marks_only_overflow() {
        for size in [1024 * 1024, 1024 * 1024 + 1] {
            let payload = format!(
                "__FTTR_BEGIN_limit__\n{}\n__FTTR_END_limit__=0\n",
                "a".repeat(size)
            );
            let (text, code, truncated) = parse_reply(payload.as_bytes(), "limit").unwrap();
            assert_eq!(text.len(), 1024 * 1024);
            assert_eq!(code, 0);
            assert_eq!(truncated, size > 1024 * 1024);
        }
    }

    #[test]
    fn mqtt_command_success_requires_a_confirmed_outcome() {
        // Given a result without a transport error but without execution proof.
        let mut result = CommandResult {
            target: "1A2B3C4DA111".into(),
            status: CommandStatus::Timeout,
            request_id: None,
            exit_code: None,
            output: None,
            output_truncated: false,
            error: None,
        };
        // When classifying each externally observable result; then unknown is not success.
        assert!(!result.success());
        result.status = CommandStatus::Executed;
        assert!(!result.success());
        result.exit_code = Some(0);
        assert!(result.success());
        result.exit_code = Some(7);
        assert!(!result.success());
        result.status = CommandStatus::Dispatched;
        result.exit_code = None;
        assert!(result.success());
    }

    #[test]
    fn shell_frame_preserves_script_and_reports_stderr_and_exit() {
        let script = "printf '%s\\n' \"single'quote\"; printf 'error\\n' >&2; exit 7";
        let result = Command::new("sh")
            .args(["-c", &framed_script(script, "test123")])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            parse_reply(&result.stdout, "test123"),
            Some(("single'quote\nerror\n".into(), 7, false))
        );
        assert!(parse_reply(&result.stdout, "previous_request").is_none());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn empty_output_still_has_a_response_and_binary_output_is_lossy_text() {
        let result = Command::new("sh")
            .args(["-c", &framed_script("exit 0", "empty")])
            .output()
            .unwrap();
        assert_eq!(
            parse_reply(&result.stdout, "empty"),
            Some((String::new(), 0, false))
        );
        assert!(parse_reply(
            b"__FTTR_BEGIN_empty__\nbody\n__FTTR_END_empty__=bad\n",
            "empty"
        )
        .is_none());
        assert_eq!(
            parse_reply(
                b"__FTTR_BEGIN_empty__\n\xff\n__FTTR_END_empty__=0\n",
                "empty"
            ),
            Some(("\u{fffd}".into(), 0, false))
        );
    }

    #[test]
    fn identity_and_advisory_lock_reject_ambiguous_commands() {
        assert_eq!(normalize_mac("1a:2b:3c:4d:a1:11").unwrap(), "1A2B3C4DA111");
        assert!(normalize_mac("H3CT1A2B3C4D").is_err());
        assert!(normalize_mac("../1A2B3C4DA111").is_err());
        let target = format!("TEST{}", request_id());
        let first = lock_target(&target).unwrap();
        assert_eq!(
            lock_target(&target).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        lock_target(&target).unwrap();
        std::fs::remove_file(format!("/tmp/fttr-command-{target}.lock")).unwrap();
    }

    #[test]
    fn command_locks_are_isolated_per_device() {
        let suffix = request_id();
        let first_target = format!("TESTA{suffix}");
        let second_target = format!("TESTB{suffix}");
        let first = lock_target(&first_target).unwrap();
        let second = lock_target(&second_target).unwrap();

        assert!(lock_target(&first_target).is_err());
        assert!(lock_target(&second_target).is_err());

        drop((first, second));
        std::fs::remove_file(format!("/tmp/fttr-command-{first_target}.lock")).unwrap();
        std::fs::remove_file(format!("/tmp/fttr-command-{second_target}.lock")).unwrap();
    }

    #[test]
    #[cfg(feature = "mqtt")]
    fn transaction_excludes_raw_exec_dispatch_and_broadcast_between_publications() {
        let target = "0200000000F1";
        let transaction = Transaction::acquire(target).unwrap();
        // No publication lock is held: these must fail at transaction admission,
        // before connecting or enqueueing anything to the broker.
        assert_eq!(
            execute(target, "fixture", Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            dispatch(target, "fixture").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            dispatch("all", "fixture").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let other = Transaction::acquire("0200000000F2").unwrap();
        assert!(Transaction::acquire("02:00:00:00:00:f1").is_err());
        drop((transaction, other));
        Transaction::acquire(target).unwrap();
    }
}

// Explicit unlock also releases descriptors briefly inherited by concurrent fork/exec.
#[cfg(any(feature = "mqtt", test))]
#[derive(Debug)]
pub(super) struct CommandLock(File);

#[cfg(any(feature = "mqtt", test))]
impl Drop for CommandLock {
    fn drop(&mut self) {
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        unsafe {
            flock(self.0.as_raw_fd(), 8);
        }
    }
}

// Advisory locks survive file reuse and are released automatically on process exit.
// Do not unlink the lock file: doing so would let clients lock different inodes.
#[cfg(any(feature = "mqtt", test))]
pub(super) fn lock_target(target: &str) -> io::Result<CommandLock> {
    lock_named(target, 2)
}

#[cfg(any(feature = "mqtt", test))]
fn lock_named(target: &str, mode: i32) -> io::Result<CommandLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(format!("/tmp/fttr-command-{target}.lock"))?;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    if unsafe { flock(file.as_raw_fd(), mode | 4) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("another fttrctl command is active for {target}"),
        ));
    }
    Ok(CommandLock(file))
}

/// A complete read/plan/write/readback transaction, shared by native sync,
/// bootstrap, account commands and standalone exec. Publication locks alone
/// would leave gaps between inventory and its dependent write.
#[cfg(any(feature = "mqtt", test))]
pub(super) struct Transaction {
    target: String,
    _global: CommandLock,
    _device: CommandLock,
    _effect: crate::effects::Permit,
    publication: Option<Publication>,
}

#[cfg(any(feature = "mqtt", test))]
struct Publication {
    selection: crate::mqtt_identity::Selection,
    epoch: u64,
}

#[cfg(any(feature = "mqtt", test))]
impl Transaction {
    pub(super) fn acquire(target: &str) -> io::Result<Self> {
        Self::acquire_inner(target, true)
    }

    // No mutable transaction permit or caller-supplied script is exposed by
    // the exception for observing an unresolved write.
    #[cfg(feature = "mqtt")]
    pub(super) async fn observe_inventory(
        target: &str,
        selection: &crate::mqtt_identity::Selection,
    ) -> io::Result<super::command_binding::SelectedResult> {
        let tx = Self::acquire_inner(target, false)?;
        let script = format!(
            "{}\n{}",
            super::provisioning::BOOT_PROBE,
            super::provisioning::INVENTORY
        );
        super::command_binding::execute_transaction_async(
            &tx,
            selection,
            &script,
            Duration::from_secs(90),
        )
        .await
    }

    #[cfg(feature = "mqtt")]
    pub(super) async fn probe_recovery(
        &self,
        selection: &crate::mqtt_identity::Selection,
        request_id: &str,
        expected_boot_id: &str,
    ) -> Result<String, (String, bool)> {
        let probe = super::provisioning::remote_status_probe(request_id, expected_boot_id)
            .map_err(|reason| (reason, false))?;
        let script = format!(
            "{}\n{}\n{}",
            super::provisioning::BOOT_PROBE,
            probe,
            super::provisioning::INVENTORY
        );
        super::provisioning::execute_once(self, selection, &script).await
    }

    #[cfg(feature = "mqtt")]
    pub(super) fn acquire_for_recovery(target: &str) -> io::Result<Self> {
        Self::acquire_inner(target, false)
    }

    #[cfg(all(feature = "mqtt", feature = "diagnostics"))]
    pub(super) async fn restart_for_recovery(
        target: &str,
        selection: &crate::mqtt_identity::Selection,
        boot_id: &str,
    ) -> io::Result<super::command_binding::SelectedResult> {
        if !super::provisioning::valid_boot_id(boot_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid observed boot ID",
            ));
        }
        let tx = Self::acquire_inner(target, false)?;
        let script = format!("read -r boot </proc/sys/kernel/random/boot_id && [ \"$boot\" = '{boot_id}' ] && command -v reboot >/dev/null && reboot");
        super::command_binding::execute_transaction_async(
            &tx,
            selection,
            &script,
            Duration::from_secs(12),
        )
        .await
    }

    fn acquire_inner(target: &str, require_settled: bool) -> io::Result<Self> {
        let _ = require_settled;
        let target = normalize_mac(target)?;
        let global = lock_named("transactions", 1)?;
        let device = lock_target(&format!("transaction-{target}"))?;
        let effect = crate::effects::Permit::acquire(crate::effects::Kind::Command)?;
        let publication = if crate::omci_auth::managed() {
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let snapshot = crate::mqtt_identity::shared()
                .lock()
                .map_err(|_| io::Error::other("identity unavailable"))?
                .snapshots(time)
                .into_iter()
                .find(|s| s.mqtt_mac == target && s.reason == "ready")
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "fresh unique optical binding required",
                    )
                })?;
            let serial = snapshot
                .optical_sn
                .ok_or_else(|| io::Error::other("optical identity unavailable"))?;
            #[cfg(feature = "mqtt")]
            if require_settled {
                super::command_binding::ensure_no_pending(&serial)?;
            }
            let epoch = crate::omci_auth::serial_epoch(&serial).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OMCI authorization required",
                )
            })?;
            if crate::omci_auth::execution_grant()
                .is_some_and(|grant| grant != (serial.clone(), epoch))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "execution authorization changed",
                ));
            }
            Some(Publication {
                selection: crate::mqtt_identity::Selection {
                    optical_sn: serial,
                    onu_id: snapshot
                        .onu_id
                        .ok_or_else(|| io::Error::other("ONU identity unavailable"))?,
                    binding_generation: snapshot.binding_generation,
                },
                epoch,
            })
        } else {
            #[cfg(not(test))]
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "commands must execute through the fttrd authority",
            ));
            #[cfg(test)]
            None
        };
        Ok(Self {
            target,
            _global: global,
            _device: device,
            _effect: effect,
            publication,
        })
    }

    pub(super) fn target(&self) -> &str {
        &self.target
    }

    #[cfg(feature = "mqtt")]
    pub(super) fn matches_selection(&self, selection: &crate::mqtt_identity::Selection) -> bool {
        self.publication.as_ref().is_none_or(|p| {
            p.selection.optical_sn == selection.optical_sn
                && p.selection.onu_id == selection.onu_id
                && p.selection.binding_generation == selection.binding_generation
        })
    }

    #[cfg(feature = "mqtt")]
    pub(super) fn enqueue(
        &self,
        session: &mut CommandSession,
        topic: &str,
        payload: &[u8],
    ) -> io::Result<()> {
        let Some(publication) = &self.publication else {
            return super::command_transport::enqueue(session, topic, payload);
        };
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let registry = crate::mqtt_identity::shared()
            .lock()
            .map_err(|_| io::Error::other("identity unavailable"))?;
        let target = registry
            .resolve(&publication.selection, time)
            .map_err(io::Error::other)?;
        if target.mqtt_mac != self.target {
            return Err(io::Error::other("transaction binding changed"));
        }
        self.enqueue_authorized(session, topic, payload)
    }
    #[cfg(feature = "mqtt")]
    pub(super) fn enqueue_authorized(
        &self,
        session: &mut CommandSession,
        topic: &str,
        payload: &[u8],
    ) -> io::Result<()> {
        let mut action = || super::command_transport::enqueue(session, topic, payload);
        match &self.publication {
            Some(grant) => {
                crate::omci_auth::enqueue_serial(&grant.selection.optical_sn, grant.epoch, action)
            }
            None => action(),
        }
    }

    #[cfg(feature = "mqtt")]
    pub(super) async fn execute(
        &self,
        script: &str,
        timeout: Duration,
    ) -> io::Result<CommandResult> {
        let _publication = lock_target(&self.target)?;
        super::command_transport::run(
            "127.0.0.1:1883".parse().unwrap(),
            super::command_transport::Request {
                target: self.target.clone(),
                script,
                id: request_id(),
                timeout,
            },
            |session, topic, payload| self.enqueue(session, topic, payload),
        )
        .await
    }
}

/// QoS 0 dispatch only. Success reports a socket write, not device execution.
#[cfg(feature = "mqtt")]
pub fn dispatch(target: &str, command: &str) -> io::Result<CommandResult> {
    // Unconfirmed dispatch has no transaction completion contract. Managed
    // callers use per-device framed exec; a broadcast cannot be authorized by
    // one device's identity or epoch.
    #[cfg(not(test))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "unconfirmed dispatch disabled; use daemon-authorized exec",
    ));
    #[cfg(test)]
    {
        let target = if target == "all" {
            "all".to_string()
        } else {
            normalize_mac(target)?
        };
        if command.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "command must not be empty",
            ));
        }
        // A broadcast conflicts with every per-device transaction. Unicast takes
        // the same transaction gate as exec, not merely its publication lock.
        let _broadcast;
        let _transaction;
        if target == "all" {
            _broadcast = Some(lock_named("transactions", 2)?);
            _transaction = None;
        } else {
            _broadcast = None;
            _transaction = Some(Transaction::acquire(&target)?);
        }
        let _lock = lock_target(&target)?;
        let _effect = if target == "all" {
            Some(crate::effects::Permit::acquire(
                crate::effects::Kind::Command,
            )?)
        } else {
            None
        };
        super::command_transport::blocking(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let mut session = CommandSession::connect(
                "127.0.0.1:1883".parse().unwrap(),
                &format!("fttr-cmd-{}", request_id()),
                deadline,
            )
            .await
            .map_err(io::Error::other)?;
            session
                .enqueue_publish(&format!("/fttr/subdev/cmd/{target}"), command.as_bytes())
                .map_err(io::Error::other)?;
            session.flushed(deadline).await.map_err(io::Error::other)
        })?;
        Ok(CommandResult {
            target,
            status: CommandStatus::Dispatched,
            request_id: None,
            exit_code: None,
            output: None,
            output_truncated: false,
            error: None,
        })
    }
}

/// Wait for a fresh, matching response frame containing combined output and exit code.
#[cfg(feature = "mqtt")]
pub fn execute(target: &str, command: &str, timeout: Duration) -> io::Result<CommandResult> {
    crate::runtime::block_on(execute_async(target, command, timeout))
}

#[cfg(feature = "mqtt")]
pub async fn execute_async(
    target: &str,
    command: &str,
    timeout: Duration,
) -> io::Result<CommandResult> {
    let target = normalize_mac(target)?;
    if command.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command must not be empty",
        ));
    }
    Transaction::acquire(&target)?
        .execute(command, timeout)
        .await
}

#[cfg(not(feature = "mqtt"))]
pub fn dispatch(_: &str, _: &str) -> io::Result<CommandResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MQTT commands unavailable: build requires mqtt",
    ))
}

#[cfg(not(feature = "mqtt"))]
pub fn execute(_: &str, _: &str, _: Duration) -> io::Result<CommandResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MQTT commands unavailable: build requires mqtt",
    ))
}
