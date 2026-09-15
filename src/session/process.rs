use crate::error::BackendError;
use nix::{
    sys::{
        signal::{Signal, kill, killpg},
        wait::{WaitPidFlag, waitpid},
    },
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub started: u64,
}

impl ProcessIdentity {
    pub fn read(pid: i32) -> Option<Self> {
        let stat = procfs::process::Process::new(pid).ok()?.stat().ok()?;
        if matches!(stat.state, 'Z' | 'X') {
            return None;
        }
        Some(Self {
            pid,
            started: stat.starttime,
        })
    }
    pub fn alive(&self) -> bool {
        Self::read(self.pid).is_some_and(|current| current.started == self.started)
    }
    pub fn signal(&self, signal: i32) {
        if self.alive()
            && let Ok(signal) = Signal::try_from(signal)
        {
            let _ = killpg(Pid::from_raw(self.pid), signal);
        }
    }
    pub fn signal_process(&self, signal: i32) {
        if self.alive()
            && let Ok(signal) = Signal::try_from(signal)
        {
            let _ = kill(Pid::from_raw(self.pid), signal);
        }
    }

    pub fn with_environment(key: &str, value: &str) -> Vec<Self> {
        let Ok(processes) = procfs::process::all_processes() else {
            return vec![];
        };
        processes
            .flatten()
            .filter_map(|process| {
                let pid = process.pid();
                if pid == std::process::id() as i32 {
                    return None;
                }
                let identity = Self::read(pid)?;
                let environment = process.environ().ok()?;
                let matches = environment
                    .iter()
                    .any(|(environment_key, environment_value)| {
                        environment_key.to_string_lossy() == key
                            && environment_value.to_string_lossy() == value
                    });
                (matches && identity.alive()).then_some(identity)
            })
            .collect()
    }
}

pub fn terminate_owned(processes: &[ProcessIdentity], runtime: &str) {
    if runtime.is_empty() {
        return;
    }
    let members = ProcessIdentity::with_environment("XDG_RUNTIME_DIR", runtime);
    for process in &members {
        process.signal_process(libc::SIGTERM);
    }
    for process in processes.iter().rev() {
        process.signal(libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while processes.iter().chain(&members).any(ProcessIdentity::alive) && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    for process in processes.iter().rev() {
        process.signal(libc::SIGKILL);
    }
    for process in members.iter().chain(&ProcessIdentity::with_environment(
        "XDG_RUNTIME_DIR",
        runtime,
    )) {
        process.signal_process(libc::SIGKILL);
    }
    for process in processes.iter().chain(&members) {
        let _ = waitpid(Pid::from_raw(process.pid), Some(WaitPidFlag::WNOHANG));
    }
}

pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self, BackendError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| BackendError::Io {
                path: path.display().to_string(),
                error: error.to_string(),
            })?;
        fs4::FileExt::try_lock(&file).map_err(|_| {
            BackendError::Failed(format!("Another controller owns {}", path.display()))
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_matches_owner_and_process_start_time() {
        let owner = uuid::Uuid::new_v4().to_string();
        let mut owned = tokio::process::Command::new("sleep")
            .arg("30")
            .env("AGENT_DESKTOP_PROCESS_TEST", &owner)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut unrelated = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let matches = ProcessIdentity::with_environment("AGENT_DESKTOP_PROCESS_TEST", &owner);
        assert_eq!(matches.len(), 1);
        let identity = &matches[0];
        assert_eq!(identity.pid, owned.id().unwrap() as i32);
        let stale = ProcessIdentity {
            pid: identity.pid,
            started: identity.started + 1,
        };
        stale.signal_process(libc::SIGKILL);
        assert!(owned.try_wait().unwrap().is_none());
        identity.signal_process(libc::SIGTERM);
        tokio::time::timeout(std::time::Duration::from_secs(2), owned.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    }
}
