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

/// Seat parameters. Seat 0 keeps the legacy names (wayland-virtual,
/// agent-desktop-virtual.env, VNC 5910); seats ≥1 are numbered.
#[derive(Debug, Clone, Copy)]
pub struct SeatParams {
    pub id: u32,
    pub width: u32,
    pub height: u32,
}

impl SeatParams {
    pub fn socket_name(self) -> String {
        if self.id == 0 {
            "wayland-virtual".to_string()
        } else {
            format!("wayland-virtual-{}", self.id)
        }
    }

    pub fn default_env_file(self) -> String {
        if self.id == 0 {
            "/tmp/agent-desktop-virtual.env".to_string()
        } else {
            format!("/tmp/agent-desktop-virtual-{}.env", self.id)
        }
    }

    pub fn vnc_port(self) -> u16 {
        5910 + self.id.min(90) as u16
    }
}

/// Handle to a booted virtual seat. Kills children on [`shutdown`].
pub struct VirtualSeat {
    pids: Vec<i32>,
    pub bus_address: String,
    pub wayland_display: String,
    pub env_file: String,
    seat_runtime: std::path::PathBuf,
}

impl VirtualSeat {
    /// Boot: dbus-launch → kwin_wayland --virtual → at-spi2-registryd →
    /// wayvnc (best-effort). Exports the new bus/display into this process's
    /// env so all drivers bind to the virtual seat, and writes an env file
    /// so out-of-process launchers (ssh, CI) can join the same seat.
    pub async fn boot(width: u32, height: u32) -> Result<Self, BackendError> {
        Self::boot_with(SeatParams { id: 0, width, height }).await
    }

    pub async fn boot_with(params: SeatParams) -> Result<Self, BackendError> {
        // Per-SEAT runtime dir (by seat id, NOT pid — farm boots N seats from
        // one process). at-spi sockets live under $XDG_RUNTIME_DIR; sharing
        // one dir across seats makes launchers fight over at-spi/bus and
        // clients see GUID chaos. Never the live /run/user/$UID seat.
        let seat_runtime = std::env::temp_dir().join(format!("ad-seat-{}-run", params.id));
        std::fs::create_dir_all(&seat_runtime).map_err(|e| BackendError::Io {
            path: seat_runtime.to_string_lossy().to_string(),
            error: e.to_string(),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &seat_runtime,
                std::fs::Permissions::from_mode(0o700),
            );
        }
        // SAFETY: bootstrap runs before any tool dispatch reads env.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &seat_runtime);
        }

        let out = Command::new("dbus-launch")
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
        let wayland_display = params.socket_name();
        // SAFETY: bootstrap runs before any tool dispatch reads env.
        // (XDG_RUNTIME_DIR already points at the per-seat dir; dbus-launch
        // inherited it for its daemon.)
        unsafe {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus_address);
            std::env::set_var("WAYLAND_DISPLAY", &wayland_display);
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
            std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
        }

        let mut pids = Vec::new();
        // Track the bus daemon itself so boots never leak it.
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
                &params.width.to_string(),
                "--height",
                &params.height.to_string(),
                "--xwayland",
            ],
            &bus_address,
            &wayland_display,
        )?;
        pids.push(kwin);
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // AT-SPI stack, both halves pre-spawned with absolute paths: on
        // SELinux-enforcing hosts the nested dbus-daemon (unconfined_dbusd_t)
        // is denied exec of gnome_atspi_exec_t, so D-Bus activation can never
        // start them — pre-spawn instead (no activation hop at all).
        // --screen-reader=1 advertises a screen reader so lazy toolkits
        // (AccessKit-based) expose their trees too.
        match spawn_child(
            "at-spi-bus-launcher",
            &["--launch-immediately", "--a11y=1", "--screen-reader=1"],
            &bus_address,
            &wayland_display,
        ) {
            Ok(p) => pids.push(p),
            Err(e) => tracing::warn!("at-spi-bus-launcher failed: {}", e.tool(false).message),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        match spawn_child(
            "at-spi2-registryd",
            &[],
            &bus_address,
            &wayland_display,
        ) {
            Ok(p) => pids.push(p),
            Err(e) => tracing::warn!("at-spi2-registryd failed: {}", e.tool(false).message),
        }
        let vnc_port = std::env::var("AGENT_DESKTOP_VNC_PORT")
            .ok()
            .filter(|v| v.parse::<u16>().is_ok())
            .unwrap_or_else(|| params.vnc_port().to_string());
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
            .unwrap_or_else(|_| params.default_env_file());
        let contents = format!(
            "DBUS_SESSION_BUS_ADDRESS={bus_address}\nWAYLAND_DISPLAY={wayland_display}\nXDG_SESSION_TYPE=wayland\nXDG_CURRENT_DESKTOP=KDE\nXDG_RUNTIME_DIR={}\n",
            seat_runtime.to_string_lossy(),
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
            seat_runtime,
        })
    }

    pub fn shutdown(self) {
        Self::kill_all(&self.pids);
        let _ = std::fs::remove_dir_all(&self.seat_runtime);
    }

    /// Release ownership of child pids without killing (farm table owns them).
    pub fn into_pids(self) -> Vec<i32> {
        let pids = self.pids.clone();
        std::mem::forget(self);
        pids
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

fn path_has(program: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join(program).is_file())
}

/// Join an existing seat: read its env file and adopt bus/display/session
/// into this process. Used by `--join-seat` servers and out-of-process
/// launchers. No processes are spawned and nothing is owned.
pub fn join_seat(env_file: &str) -> Result<(), BackendError> {
    let contents = std::fs::read_to_string(env_file).map_err(|e| BackendError::Io {
        path: env_file.into(),
        error: e.to_string(),
    })?;
    // SAFETY: join happens at startup before any tool dispatch reads env.
    unsafe {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                std::env::set_var(k.trim(), v.trim());
            }
        }
    }
    Ok(())
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
