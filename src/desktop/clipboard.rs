use crate::{error::fail, platform::drivers::SessionType, types::ToolError};
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone)]
pub struct Clipboard {
    session: SessionType,
    previous: Option<String>,
    inserted: String,
    armed: bool,
}
async fn output(program: &str, args: &[&str]) -> Result<std::process::Output, ToolError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| fail("clipboard_timeout", "Clipboard provider timed out"))?
    .map_err(|error| fail("clipboard_unavailable", error.to_string()))
}
async fn current(session: SessionType) -> Result<Option<String>, ToolError> {
    let types = match session {
        SessionType::Wayland => output("wl-paste", &["--list-types"]).await?,
        SessionType::X11 => {
            output(
                "xclip",
                &["-o", "-selection", "clipboard", "-target", "TARGETS"],
            )
            .await?
        }
    };
    if types.status.success()
        && String::from_utf8_lossy(&types.stdout).lines().any(|mime| {
            !matches!(
                mime.trim(),
                "TARGETS"
                    | "TIMESTAMP"
                    | "MULTIPLE"
                    | "SAVE_TARGETS"
                    | "UTF8_STRING"
                    | "STRING"
                    | "TEXT"
                    | "COMPOUND_TEXT"
            ) && !mime.trim().starts_with("text/plain")
        })
    {
        return Err(fail(
            "unsupported",
            "Clipboard contains formats this provider cannot restore; use a semantic text edit",
        ));
    }
    let out = match session {
        SessionType::Wayland => output("wl-paste", &["--no-newline", "--type", "text"]).await?,
        SessionType::X11 => {
            output(
                "xclip",
                &["-o", "-selection", "clipboard", "-target", "UTF8_STRING"],
            )
            .await?
        }
    };
    if !out.status.success() {
        if types.status.success() && !types.stdout.is_empty() {
            return Err(fail(
                "unsupported",
                "Clipboard contains non-text data; cannot safely replace it for paste",
            ));
        }
        return Ok(None);
    }
    String::from_utf8(out.stdout)
        .map(Some)
        .map_err(|error| fail("clipboard_unavailable", error.to_string()))
}
pub async fn set(session: SessionType, text: &str) -> Result<(), ToolError> {
    let mut command = match session {
        SessionType::Wayland => {
            let mut command = Command::new("wl-copy");
            command.args(["--type", "text/plain;charset=utf-8"]);
            command
        }
        SessionType::X11 => {
            let mut command = Command::new("xclip");
            command.args(["-selection", "clipboard", "-target", "UTF8_STRING"]);
            command
        }
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| fail("clipboard_unavailable", error.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|error| fail("clipboard_failed", error.to_string()))?;
    }
    let status = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
        .await
        .map_err(|_| fail("clipboard_timeout", "Clipboard writer timed out"))?
        .map_err(|error| fail("clipboard_failed", error.to_string()))?;
    if !status.success() {
        return Err(fail(
            "clipboard_failed",
            format!("Clipboard writer exited {status}"),
        ));
    }
    Ok(())
}
impl Clipboard {
    pub async fn replace(session: SessionType, text: &str) -> Result<Self, ToolError> {
        let previous = current(session).await?;
        let guard = Self {
            session,
            previous,
            inserted: text.into(),
            armed: true,
        };
        set(session, text).await?;
        Ok(guard)
    }
    async fn restore_inner(&self) -> Result<(), ToolError> {
        if current(self.session).await?.as_deref() != Some(self.inserted.as_str()) {
            return Ok(());
        }
        match &self.previous {
            Some(text) => set(self.session, text).await,
            None => match self.session {
                SessionType::Wayland => {
                    output("wl-copy", &["--clear"]).await?;
                    Ok(())
                }
                SessionType::X11 => set(self.session, "").await,
            },
        }
    }
    pub async fn restore(mut self) -> Result<(), ToolError> {
        let result = self.restore_inner().await;
        self.armed = false;
        result
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        if self.armed {
            let mut restore = self.clone();
            restore.armed = false;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = restore.restore_inner().await;
                });
            }
        }
    }
}
