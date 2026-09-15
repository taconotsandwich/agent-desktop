use crate::error::BackendError;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// Register only inside the explicitly joined session's private data directory.
/// Live desktops use the application entry installed by packaging/install.sh.
pub async fn authorize_private_session() -> Result<(), BackendError> {
    let (Some(runtime), Some(data), Some(cache)) = (
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("XDG_CACHE_HOME"),
    ) else {
        return Ok(());
    };
    let Some(data) = private_directory(Path::new(&runtime), Path::new(&data))? else {
        return Ok(());
    };
    if private_directory(Path::new(&runtime), Path::new(&cache))?.is_none() {
        return Ok(());
    }
    let executable = std::fs::canonicalize(std::env::current_exe().map_err(io)?).map_err(io)?;
    register_application(&data, &executable)?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("kbuildsycoca6")
            .arg("--noincremental")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| BackendError::Timeout {
        detail: "Updating the private KDE application's screenshot authorization".into(),
    })?
    .map_err(io)?;
    if !output.status.success() {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!(
                "KDE application registration failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    Ok(())
}

fn private_directory(runtime: &Path, data: &Path) -> Result<Option<PathBuf>, BackendError> {
    let runtime = std::fs::canonicalize(runtime).map_err(io)?;
    let data = std::fs::canonicalize(data).map_err(io)?;
    Ok((data != runtime && data.starts_with(runtime)).then_some(data))
}

pub fn register_application(data: &Path, executable: &Path) -> Result<(), BackendError> {
    let contents = application_entry(executable)?;
    let directory = data.join("applications");
    std::fs::create_dir_all(&directory).map_err(io)?;
    let mut entry = tempfile::NamedTempFile::new_in(&directory).map_err(io)?;
    entry.write_all(contents.as_bytes()).map_err(io)?;
    entry
        .persist(directory.join("agent-desktop.desktop"))
        .map_err(|error| io(error.error))?;
    Ok(())
}

pub fn application_entry(executable: &Path) -> Result<String, BackendError> {
    let path = executable
        .to_str()
        .ok_or_else(|| BackendError::Unsupported {
            reason: "KDE application registration requires a UTF-8 executable path".into(),
        })?;
    let command = format!(
        "\"{}\"",
        path.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%")
    );
    let mut entry = String::new();
    for line in include_str!("../../../packaging/agent-desktop.desktop").lines() {
        let line = if line.starts_with("Exec=") {
            format!("Exec={}", desktop_string(&command))
        } else if line.starts_with("TryExec=") {
            format!("TryExec={}", desktop_string(path))
        } else {
            line.into()
        };
        entry.push_str(&line);
        entry.push('\n');
    }
    Ok(entry)
}

fn desktop_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn io(error: std::io::Error) -> BackendError {
    BackendError::Failed(format!("KDE application registration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_cannot_write_outside_the_private_session() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("session");
        let private = runtime.join("data");
        let outside = root.path().join("host-applications");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, runtime.join("external-data")).unwrap();
        assert!(private_directory(&runtime, &private).unwrap().is_some());
        for data in [&runtime, &outside, &runtime.join("external-data")] {
            assert!(private_directory(&runtime, data).unwrap().is_none());
        }
    }
}
