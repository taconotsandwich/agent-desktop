mod input;
mod screenshot;
mod windows;
pub use input::X11Input;
pub use screenshot::X11Shot;
pub use windows::X11Windows;

use crate::error::BackendError;
use tokio::process::Command;

pub(super) fn display() -> Result<String, BackendError> {
    std::env::var("DISPLAY").map_err(|_| BackendError::Unavailable {
        backend: "x11",
        detail: "DISPLAY not set".into(),
    })
}

pub(super) fn has(bin: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join(bin).is_file())
}

pub(super) async fn run(
    what: &'static str,
    cmd: &str,
    args: &[&str],
    display: &str,
) -> Result<Vec<u8>, BackendError> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new(cmd)
            .args(args)
            .env("DISPLAY", display)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| BackendError::Timeout {
        detail: format!("{what}: {cmd} timed out"),
    })?
    .map_err(|e| BackendError::Io {
        path: cmd.into(),
        error: e.to_string(),
    })?;
    if !out.status.success() {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!(
                "{what}: {} exited {}: {}",
                cmd,
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    if !out.stderr.is_empty() {
        tracing::debug!(command = cmd, stderr = %String::from_utf8_lossy(&out.stderr), "X11 command diagnostic");
    }
    Ok(out.stdout)
}
