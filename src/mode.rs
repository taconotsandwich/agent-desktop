//! Seat modes: live (the user's real desktop) vs virtual (an isolated
//! `kwin_wayland --virtual` compositor that never touches the live seat).
//!
//! One mode per process (kde-mcp ADR-0003): the mode is chosen at startup,
//! all drivers bind to that mode's bus, no runtime swapping. Live mode may
//! steal focus — that is inherent to sharing a seat. Anything that must not
//! disturb the user runs in virtual mode.

use crate::error::BackendError;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Virtual,
}

impl Mode {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "virtual" => Mode::Virtual,
            _ => Mode::Live,
        }
    }
}

/// Handle to a booted virtual seat. Kills children on [`shutdown`].
pub struct VirtualSeat {
    pids: Vec<i32>,
    pub bus_address: String,
    pub wayland_display: String,
    pub env_file: String,
}

impl VirtualSeat {
    /// Boot: dbus-launch → kwin_wayland --virtual → at-spi2-registryd →
    /// wayvnc (best-effort). Exports the new bus/display into this process's
    /// env so all drivers bind to the virtual seat, and writes an env file
    /// so out-of-process launchers (ssh, CI) can join the same seat.
    pub async fn boot(width: u32, height: u32) -> Result<Self, BackendError> {
        let out = Command::new("dbus-launch")
            .arg("--sh-syntax")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| BackendError::ExternalCommandFailed {
                stderr: format!("dbus-launch: {e}"),
            })?;
        if !out.status.success() {
            return Err(BackendError::ExternalCommandFailed {
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            });
        }
        let sh = String::from_utf8_lossy(&out.stdout);
        let bus_address = parse_dbus_address(&sh).ok_or_else(|| BackendError::ExternalCommandFailed {
            stderr: "could not parse DBUS_SESSION_BUS_ADDRESS".into(),
        })?;
        let mut pids = Vec::new();
        // Track the bus daemon itself so boots never leak it.
        if let Some(pid) = parse_dbus_pid(&sh) {
            pids.push(pid);
        }
        let wayland_display = "wayland-virtual".to_string();
        // Activation overlay: the stock org.a11y.Bus.service carries
        // SystemdService=at-spi-dbus-bus.service, and on a private
        // dbus-launch bus systemd activation fails with Permission denied
        // (no user-session link). Shadow it with an Exec-only entry via
        // XDG_DATA_DIRS so dbus-daemon falls back to traditional spawn.
        let overlay = std::env::temp_dir().join(format!("ad-virt-{}", std::process::id()));
        let svc_dir = overlay.join("dbus-1").join("services");
        std::fs::create_dir_all(&svc_dir).map_err(|e| BackendError::Io {
            path: svc_dir.to_string_lossy().to_string(),
            error: e.to_string(),
        })?;
        std::fs::write(
            svc_dir.join("org.a11y.Bus.service"),
            "[D-BUS Service]\nName=org.a11y.Bus\nExec=/usr/libexec/at-spi-bus-launcher\n",
        )
        .map_err(|e| BackendError::Io {
            path: "org.a11y.Bus.service".into(),
            error: e.to_string(),
        })?;
        let overlay_dirs = format!(
            "{}:/usr/local/share:/usr/share",
            overlay.to_string_lossy()
        );
        // SAFETY: bootstrap runs before any tool dispatch reads env.
        unsafe {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus_address);
            std::env::set_var("WAYLAND_DISPLAY", &wayland_display);
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
            std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
            std::env::set_var("XDG_DATA_DIRS", &overlay_dirs);
            if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
                #[cfg(unix)]
                std::env::set_var(
                    "XDG_RUNTIME_DIR",
                    format!("/run/user/{}", nix_uid()),
                );
            }
        }

        let mut pids = Vec::new();
        if let Some(pid) = parse_dbus_pid(&sh) {
            pids.push(pid);
        }
        let kwin = spawn_child(
            "kwin_wayland",
            &[
                "--virtual",
                "--socket",
                &wayland_display,
                "--width",
                &width.to_string(),
                "--height",
                &height.to_string(),
                "--xwayland",
            ],
            &bus_address,
            &wayland_display,
        )?;
        pids.push(kwin);
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // AT-SPI bus launcher (owns org.a11y.Bus, starts registryd). Works
        // now that the overlay routes activation to plain Exec spawn.
        match spawn_child(
            "at-spi-bus-launcher",
            &["--launch-immediately"],
            &bus_address,
            &wayland_display,
        ) {
            Ok(p) => pids.push(p),
            Err(e) => tracing::warn!("at-spi-bus-launcher failed: {}", e.tool(false).message),
        }
        let vnc_port = std::env::var("AGENT_DESKTOP_VNC_PORT").unwrap_or_else(|_| "5910".into());
        match spawn_child(
            "wayvnc",
            &["127.0.0.1", &vnc_port],
            &bus_address,
            &wayland_display,
        ) {
            Ok(p) => {
                pids.push(p);
                tracing::info!(port = %vnc_port, "wayvnc readback on virtual seat");
            }
            Err(e) => tracing::warn!("wayvnc failed: {}", e.tool(false).message),
        }

        let env_file = std::env::var("AGENT_DESKTOP_ENV_FILE")
            .unwrap_or_else(|_| "/tmp/agent-desktop-virtual.env".into());
        let contents = format!(
            "DBUS_SESSION_BUS_ADDRESS={bus_address}\nWAYLAND_DISPLAY={wayland_display}\nXDG_SESSION_TYPE=wayland\nXDG_CURRENT_DESKTOP=KDE\nXDG_DATA_DIRS={overlay_dirs}\n"
        );
        std::fs::write(&env_file, contents).map_err(|e| BackendError::Io {
            path: env_file.clone(),
            error: e.to_string(),
        })?;
        Ok(Self {
            pids,
            bus_address,
            wayland_display,
            env_file,
        })
    }

    pub fn shutdown(self) {
        Self::kill_all(&self.pids);
    }

    fn kill_all(pids: &[i32]) {
        for pid in pids {
            unsafe {
                libc::kill(*pid, libc::SIGTERM);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
        for pid in pids {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for VirtualSeat {
    fn drop(&mut self) {
        // Best-effort: no sleep here, SIGTERM only. Graceful path is shutdown().
        for pid in &self.pids {
            unsafe {
                libc::kill(*pid, libc::SIGTERM);
            }
        }
    }
}

fn spawn_child(
    program: &str,
    args: &[&str],
    bus_address: &str,
    wayland_display: &str,
) -> Result<i32, BackendError> {
    // Some helpers (at-spi-bus-launcher) live outside PATH.
    let resolved = if program.contains('/') || path_has(program) {
        program.to_string()
    } else {
        format!("/usr/libexec/{program}")
    };
    let mut cmd = Command::new(&resolved);
    cmd.args(args);
    cmd.env("DBUS_SESSION_BUS_ADDRESS", bus_address);
    cmd.env("WAYLAND_DISPLAY", wayland_display);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    // SAFETY: pre_exec runs between fork and exec; setsid is signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map(|c| c.id() as i32)
        .map_err(|e| BackendError::ExternalCommandFailed {
            stderr: format!("spawn {program}: {e}"),
        })
}

#[cfg(unix)]
fn nix_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn path_has(program: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join(program).is_file())
}

fn parse_dbus_address(sh_output: &str) -> Option<String> {
    for line in sh_output.lines() {
        if let Some(rest) = line.strip_prefix("DBUS_SESSION_BUS_ADDRESS=") {
            let cleaned = rest
                .trim_end_matches(';')
                .trim_matches('\'')
                .trim_matches('"');
            return Some(cleaned.to_string());
        }
    }
    None
}

fn parse_dbus_pid(sh_output: &str) -> Option<i32> {
    for line in sh_output.lines() {
        if let Some(rest) = line.strip_prefix("DBUS_SESSION_BUS_PID=") {
            return rest.trim_end_matches(';').parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dbus_launch_sh_syntax() {
        let sample = "DBUS_SESSION_BUS_ADDRESS='unix:abstract=/tmp/dbus-abc,guid=def';\nexport DBUS_SESSION_BUS_ADDRESS;\n";
        assert_eq!(
            parse_dbus_address(sample).unwrap(),
            "unix:abstract=/tmp/dbus-abc,guid=def"
        );
    }

    #[test]
    fn missing_address_returns_none() {
        assert!(parse_dbus_address("DBUS_SESSION_PID=42").is_none());
    }
}
