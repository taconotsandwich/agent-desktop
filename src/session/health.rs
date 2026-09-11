use super::process::ProcessIdentity;
use serde::Serialize;
use std::{path::Path, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ready,
    Degraded,
    Stopped,
}

#[derive(Debug, Serialize)]
pub struct SeatHealth {
    pub health: Health,
    pub dead_pids: Vec<i32>,
    pub issues: Vec<String>,
}

pub async fn inspect(
    processes: &[ProcessIdentity],
    runtime: &str,
    display: &str,
    bus_address: &str,
) -> SeatHealth {
    let dead_pids: Vec<_> = processes
        .iter()
        .filter(|process| !process.alive())
        .map(|process| process.pid)
        .collect();
    let mut issues = Vec::new();
    if processes.is_empty() {
        issues.push("No tracked session helpers".into());
    } else if !dead_pids.is_empty() {
        issues.push(format!("Session helpers exited: {dead_pids:?}"));
    }
    if dead_pids.len() == processes.len() {
        let has_members = !ProcessIdentity::with_environment("XDG_RUNTIME_DIR", runtime).is_empty();
        if has_members {
            issues.push("Owned helper processes remain after the session exited".into());
        }
        return SeatHealth {
            health: if has_members {
                Health::Degraded
            } else {
                Health::Stopped
            },
            dead_pids,
            issues,
        };
    }
    if let Err(error) = probe(runtime, display, bus_address).await {
        issues.push(error);
    }
    SeatHealth {
        health: if issues.is_empty() {
            Health::Ready
        } else {
            Health::Degraded
        },
        dead_pids,
        issues,
    }
}

async fn probe(runtime: &str, display: &str, bus_address: &str) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let socket = Path::new(runtime).join(display);
        tokio::net::UnixStream::connect(&socket)
            .await
            .map_err(|error| {
                format!(
                    "Wayland socket {} is unavailable: {error}",
                    socket.display()
                )
            })?;
        let bus = zbus::connection::Builder::address(bus_address)
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| format!("Session bus unavailable: {error}"))?;
        let kwin = zbus::Proxy::new(&bus, "org.kde.KWin", "/KWin", "org.kde.KWin")
            .await
            .map_err(|error| error.to_string())?;
        let _: String = kwin
            .call("supportInformation", &())
            .await
            .map_err(|error| format!("KWin is not ready: {error}"))?;
        let launcher = zbus::Proxy::new(&bus, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
            .await
            .map_err(|error| error.to_string())?;
        let address: String = launcher
            .call("GetAddress", &())
            .await
            .map_err(|error| format!("Accessibility bus unavailable: {error}"))?;
        let accessibility = zbus::connection::Builder::address(address.as_str())
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| error.to_string())?;
        let dbus = zbus::fdo::DBusProxy::new(&accessibility)
            .await
            .map_err(|error| error.to_string())?;
        let registry =
            zbus::names::BusName::try_from("org.a11y.atspi.Registry").expect("static bus name");
        if !dbus
            .name_has_owner(registry)
            .await
            .map_err(|error| error.to_string())?
        {
            return Err("Accessibility registry is not ready".into());
        }
        Ok(())
    })
    .await
    .map_err(|_| "Session readiness probe timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_helpers_without_a_compositor_are_degraded() {
        let runtime = tempfile::tempdir().unwrap();
        let identity = ProcessIdentity::read(std::process::id() as i32).unwrap();
        let report = inspect(
            &[identity],
            runtime.path().to_str().unwrap(),
            "missing-wayland",
            "unix:path=/missing",
        )
        .await;
        assert_eq!(report.health, Health::Degraded);
        assert!(report.dead_pids.is_empty());
        assert!(report.issues[0].contains("Wayland socket"));
    }

    #[tokio::test]
    async fn stale_process_identity_cannot_report_a_ready_seat() {
        let runtime = tempfile::tempdir().unwrap();
        let mut identity = ProcessIdentity::read(std::process::id() as i32).unwrap();
        identity.started += 1;
        let report = inspect(
            &[identity],
            runtime.path().to_str().unwrap(),
            "wayland",
            "unix:path=/missing",
        )
        .await;
        assert_eq!(report.health, Health::Stopped);
        assert_eq!(report.dead_pids, vec![std::process::id() as i32]);
    }
}
