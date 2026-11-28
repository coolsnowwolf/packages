//! Shared bounded command runner used by the offload reconciler.
pub use crate::platform::openwrt::command::run_sync;
use std::{io, time::Duration};

/// Bounded asynchronous process execution with timeout.
/// Killing a dropped child ensures task cancellation doesn't leak orphan processes.
pub async fn run_async(program: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let output = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{program} timed out after {timeout:?}"),
        )
    })??;

    if !output.status.success() {
        return Err(io::Error::other(format!("{program}: {}", output.status)));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}
