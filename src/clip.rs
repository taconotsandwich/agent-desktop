//! Clipboard helpers — one impl per display protocol, shared by all backs.
//!
//! `wl-copy` daemonises and holds stdio: never block-read its pipes; use
//! `status()` with a timeout instead of `output()`.

use crate::drivers::SessionType;
use crate::error::BackendError;
use tokio::process::Command;

async fn run(cmd: &str, args: &[&str], stdin_bytes: Option<Vec<u8>>) -> Result<String, BackendError> {
    let mut c = Command::new(cmd);
    c.args(args)
        .stdin(if stdin_bytes.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = c.spawn().map_err(|e| BackendError::Io {
        path: cmd.into(),
        error: e.to_string(),
    })?;
    if let Some(bytes) = stdin_bytes {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&bytes).await;
        }
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .map_err(|_| BackendError::Timeout {
            detail: format!("{cmd} timed out"),
        })?
        .map_err(|e| BackendError::Io {
            path: cmd.into(),
            error: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("{cmd} exited {}", out.status),
        });
    }
    String::from_utf8(out.stdout).map_err(|e| BackendError::ExternalCommandFailed {
        stderr: format!("{cmd} non-utf8 output: {e}"),
    })
}

pub async fn get(session: SessionType) -> Result<String, BackendError> {
    match session {
        SessionType::Wayland => run("wl-paste", &["--no-newline"], None).await,
        SessionType::X11 => run("xclip", &["-o", "-sel", "clip"], None).await,
    }
}

pub async fn set(session: SessionType, text: &str) -> Result<(), BackendError> {
    match session {
        SessionType::Wayland => {
            run("wl-copy", &[], Some(text.as_bytes().to_vec())).await?;
        }
        SessionType::X11 => {
            run("xclip", &["-i", "-sel", "clip"], Some(text.as_bytes().to_vec())).await?;
        }
    }
    Ok(())
}
