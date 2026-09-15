use super::{environment::read_environment, io};
use crate::{error::BackendError, session::process::ProcessIdentity};
use std::{
    collections::BTreeMap,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone, Copy)]
pub struct SeatParams {
    pub id: u32,
    pub width: u32,
    pub height: u32,
}

impl SeatParams {
    pub fn socket_name(self) -> String {
        format!("wayland-agent-{}", self.id)
    }
}

pub struct VirtualSeat {
    processes: Vec<ProcessIdentity>,
    pub bus_address: String,
    pub wayland_display: String,
    pub env_file: String,
    seat_runtime: PathBuf,
}

impl VirtualSeat {
    pub async fn boot(width: u32, height: u32) -> Result<Self, BackendError> {
        Self::boot_with(SeatParams {
            id: 0,
            width,
            height,
        })
        .await
    }

    pub async fn boot_with(params: SeatParams) -> Result<Self, BackendError> {
        if !(320..=7680).contains(&params.width) || !(240..=4320).contains(&params.height) {
            return Err(BackendError::Failed(
                "Virtual dimensions must be 320..7680 by 240..4320".into(),
            ));
        }
        let runtime = tempfile::Builder::new()
            .prefix("agent-desktop-seat-")
            .tempdir()
            .map_err(io)?;
        let seat_runtime = runtime.keep();
        let mut seat = Self {
            processes: Vec::new(),
            bus_address: String::new(),
            wayland_display: params.socket_name(),
            env_file: seat_runtime.join("session.env").display().to_string(),
            seat_runtime,
        };
        let mut env = BTreeMap::from([
            ("XDG_RUNTIME_DIR".into(), seat.runtime_dir()),
            ("WAYLAND_DISPLAY".into(), seat.wayland_display.clone()),
            ("XDG_SESSION_TYPE".into(), "wayland".into()),
            ("XDG_CURRENT_DESKTOP".into(), "KDE".into()),
            (
                "XDG_CONFIG_HOME".into(),
                seat.seat_runtime.join("config").display().to_string(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                seat.seat_runtime.join("cache").display().to_string(),
            ),
            (
                "XDG_DATA_HOME".into(),
                seat.seat_runtime.join("data").display().to_string(),
            ),
            ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON".into(), "1".into()),
            ("GTK_A11Y".into(), "atspi".into()),
        ]);
        for dir in ["config", "cache", "data"] {
            std::fs::create_dir_all(seat.seat_runtime.join(dir)).map_err(io)?;
        }
        let mut bus = tokio::process::Command::new("dbus-daemon");
        bus.args(["--session", "--nofork", "--print-address=1"])
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                std::fs::File::create(seat.seat_runtime.join("dbus.log")).map_err(io)?,
            ));
        unsafe {
            bus.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = bus.spawn().map_err(io)?;
        let identity = ProcessIdentity::read(
            child
                .id()
                .ok_or_else(|| BackendError::Failed("No bus pid".into()))? as i32,
        )
        .ok_or_else(|| BackendError::Failed("Bus exited during startup".into()))?;
        seat.processes.push(identity);
        let mut reader = BufReader::new(child.stdout.take().expect("bus stdout"));
        tokio::time::timeout(
            Duration::from_secs(5),
            reader.read_line(&mut seat.bus_address),
        )
        .await
        .map_err(|_| BackendError::Timeout {
            detail: "session bus startup".into(),
        })?
        .map_err(io)?;
        seat.bus_address = seat.bus_address.trim().into();
        if !seat.bus_address.starts_with("unix:") {
            return Err(BackendError::Failed("Invalid session bus address".into()));
        }
        env.insert("DBUS_SESSION_BUS_ADDRESS".into(), seat.bus_address.clone());
        let ready = seat.seat_runtime.join("display.env");
        let helper = seat.seat_runtime.join("session");
        std::fs::write(&helper,format!("#!/bin/sh\nprintf 'DISPLAY=%s\\nXAUTHORITY=%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > '{}'\nexec sleep infinity\n",ready.display())).map_err(io)?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
        check_socket_available(&seat.seat_runtime, &seat.wayland_display)?;
        seat.spawn(
            "kwin_wayland",
            &[
                "--virtual",
                "--socket",
                &seat.wayland_display.clone(),
                "--width",
                &params.width.to_string(),
                "--height",
                &params.height.to_string(),
                "--xwayland",
                "--no-lockscreen",
                "--no-kactivities",
                "--exit-with-session",
                &helper.display().to_string(),
            ],
            &env,
        )?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.is_file() || !seat.seat_runtime.join(&seat.wayland_display).exists() {
            if !seat.processes.iter().all(ProcessIdentity::alive) {
                return Err(seat.startup_error("A session helper exited during KWin startup"));
            }
            if Instant::now() >= deadline {
                return Err(seat.startup_error("KWin did not become ready within 20 seconds"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for (key, value) in read_environment(&ready.display().to_string())? {
            if !value.is_empty() {
                env.insert(key, value);
            }
        }
        seat.spawn(
            "at-spi-bus-launcher",
            &["--launch-immediately", "--a11y=1", "--screen-reader=1"],
            &env,
        )?;
        let session_bus = zbus::connection::Builder::address(seat.bus_address.as_str())
            .map_err(|error| BackendError::Failed(error.to_string()))?
            .build()
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        wait_name(&session_bus, "org.a11y.Bus").await?;
        let launcher = zbus::proxy::Proxy::new(
            &session_bus,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
        )
        .await
        .map_err(|error| BackendError::Failed(error.to_string()))?;
        let address: String = launcher
            .call("GetAddress", &())
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        env.insert("AT_SPI_BUS_ADDRESS".into(), address.clone());
        let accessibility = zbus::connection::Builder::address(address.as_str())
            .map_err(|error| BackendError::Failed(error.to_string()))?
            .build()
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        let registry = zbus::names::WellKnownName::try_from("org.a11y.atspi.Registry")
            .expect("static registry name");
        let dbus = zbus::fdo::DBusProxy::new(&accessibility)
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        // KWin may have activated the registry already. Ask D-Bus for its
        // single owner instead of tracking a second process that exits.
        dbus.start_service_by_name(registry.clone(), 0)
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        let pid = dbus
            .get_connection_unix_process_id(registry.into())
            .await
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        seat.processes
            .push(ProcessIdentity::read(pid as i32).ok_or_else(|| {
                BackendError::Failed("Accessibility registry exited during startup".into())
            })?);
        std::fs::write(
            &seat.env_file,
            env.iter()
                .map(|(key, value)| format!("{key}={value}\n"))
                .collect::<String>(),
        )
        .map_err(io)?;
        std::fs::set_permissions(&seat.env_file, std::fs::Permissions::from_mode(0o600))
            .map_err(io)?;
        let health = super::health::inspect(
            &seat.processes,
            &seat.runtime_dir(),
            &seat.wayland_display,
            &seat.bus_address,
        )
        .await;
        if health.health != super::health::Health::Ready {
            return Err(seat.startup_error(&health.issues.join("; ")));
        }
        Ok(seat)
    }

    fn startup_error(&self, reason: &str) -> BackendError {
        let log =
            std::fs::read_to_string(self.seat_runtime.join("kwin_wayland.log")).unwrap_or_default();
        let tail = log
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        BackendError::Failed(format!("Seat startup failed: {reason}\n{tail}"))
    }

    fn spawn(
        &mut self,
        program: &str,
        args: &[&str],
        env: &BTreeMap<String, String>,
    ) -> Result<(), BackendError> {
        let resolved = if path_has(program) {
            program.into()
        } else {
            format!("/usr/libexec/{program}")
        };
        let log =
            std::fs::File::create(self.seat_runtime.join(format!("{program}.log"))).map_err(io)?;
        let mut command = Command::new(resolved);
        command
            .args(args)
            .envs(env)
            .env_remove("SESSION_MANAGER")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(io)?))
            .stderr(Stdio::from(log));
        if !env.contains_key("DISPLAY") {
            command.env_remove("DISPLAY");
        }
        if !env.contains_key("AT_SPI_BUS_ADDRESS") {
            command.env_remove("AT_SPI_BUS_ADDRESS");
        }
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().map_err(io)?;
        self.processes.push(
            ProcessIdentity::read(child.id() as i32)
                .ok_or_else(|| BackendError::Failed(format!("{program} exited during startup")))?,
        );
        Ok(())
    }

    pub fn runtime_dir(&self) -> String {
        self.seat_runtime.display().to_string()
    }
    pub fn identities(&self) -> Vec<ProcessIdentity> {
        self.processes.clone()
    }
    pub fn shutdown(self) {
        drop(self);
    }
    pub fn detach(mut self) -> Vec<ProcessIdentity> {
        let processes = std::mem::take(&mut self.processes);
        self.seat_runtime = PathBuf::new();
        processes
    }
}

impl Drop for VirtualSeat {
    fn drop(&mut self) {
        super::process::terminate_owned(&self.processes, &self.runtime_dir());
        if !self.seat_runtime.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.seat_runtime);
        }
    }
}

fn check_socket_available(runtime: &std::path::Path, display: &str) -> Result<(), BackendError> {
    for name in [display.to_string(), format!("{display}.lock")] {
        let path = runtime.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(BackendError::Failed(format!(
                    "Seat resource already exists: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
    }
    Ok(())
}

fn path_has(program: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join(program).is_file())
}

async fn wait_name(connection: &zbus::Connection, name: &str) -> Result<(), BackendError> {
    let dbus = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|error| BackendError::Failed(error.to_string()))?;
    let name = zbus::names::BusName::try_from(name)
        .map_err(|error| BackendError::Failed(error.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if dbus.name_has_owner(name.clone()).await.unwrap_or(false) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(BackendError::Timeout {
                detail: format!("Waiting for {name}"),
            });
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_socket_or_lock_is_rejected_without_removing_it() {
        let runtime = tempfile::tempdir().unwrap();
        for name in ["wayland-agent-0", "wayland-agent-0.lock"] {
            let path = runtime.path().join(name);
            std::fs::write(&path, "owned by another seat").unwrap();
            assert!(check_socket_available(runtime.path(), "wayland-agent-0").is_err());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "owned by another seat"
            );
            std::fs::remove_file(path).unwrap();
        }
        assert!(check_socket_available(runtime.path(), "wayland-agent-0").is_ok());
    }
}
