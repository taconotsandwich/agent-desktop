use super::health::{self, Health, SeatHealth};
use crate::{
    error::BackendError,
    session::process::{FileLock, ProcessIdentity, terminate_owned},
    session::seat::{SeatParams, VirtualSeat},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatRecord {
    pub id: u32,
    pub processes: Vec<ProcessIdentity>,
    pub bus_address: String,
    pub wayland_display: String,
    pub env_file: String,
    pub runtime_dir: String,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FarmTable {
    pub seats: Vec<SeatRecord>,
}

#[derive(Debug, Serialize)]
pub struct SeatStatus {
    #[serde(flatten)]
    pub seat: SeatRecord,
    #[serde(flatten)]
    pub status: SeatHealth,
}

#[derive(Debug, Default, Serialize)]
pub struct FarmStatus {
    pub seats: Vec<SeatStatus>,
}

impl SeatRecord {
    async fn health(&self) -> SeatHealth {
        health::inspect(
            &self.processes,
            &self.runtime_dir,
            &self.wayland_display,
            &self.bus_address,
        )
        .await
    }
}

fn directory() -> Result<PathBuf, BackendError> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            BackendError::Failed("XDG_RUNTIME_DIR is required for farm ownership".into())
        })?;
    let dir = base.join("agent-desktop-farm");
    std::fs::create_dir_all(&dir).map_err(io)?;
    Ok(dir)
}
fn io(error: std::io::Error) -> BackendError {
    BackendError::Failed(error.to_string())
}
fn load() -> Result<FarmTable, BackendError> {
    let path = directory()?.join("seats.json");
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| BackendError::Failed(format!("Invalid farm state: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FarmTable::default()),
        Err(error) => Err(io(error)),
    }
}
fn save(table: &FarmTable) -> Result<(), BackendError> {
    use std::io::Write;
    let dir = directory()?;
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(io)?;
    serde_json::to_writer_pretty(&mut file, table)
        .map_err(|error| BackendError::Failed(error.to_string()))?;
    file.flush().map_err(io)?;
    file.as_file().sync_all().map_err(io)?;
    file.persist(dir.join("seats.json"))
        .map_err(|error| io(error.error))?;
    Ok(())
}
pub async fn up(n: u32, width: u32, height: u32) -> Result<FarmStatus, BackendError> {
    if !(1..=16).contains(&n) {
        return Err(BackendError::Failed("Farm size must be 1..16".into()));
    }
    let _lock = FileLock::acquire(&directory()?.join("ownership.lock"))?;
    let mut table = load()?;
    for id in 0..n {
        if let Some(seat) = table.seats.iter().find(|seat| seat.id == id) {
            let health = seat.health().await;
            match health.health {
                Health::Ready => continue,
                Health::Degraded => {
                    return Err(BackendError::Failed(format!(
                        "Seat {id} is degraded: {}; tear it down before restarting",
                        health.issues.join("; ")
                    )));
                }
                Health::Stopped => {}
            }
        }
        for seat in table.seats.iter().filter(|seat| seat.id == id) {
            let _ = std::fs::remove_dir_all(&seat.runtime_dir);
        }
        table.seats.retain(|seat| seat.id != id);
        let seat = VirtualSeat::boot_with(SeatParams { id, width, height }).await?;
        let record = SeatRecord {
            id,
            bus_address: seat.bus_address.clone(),
            wayland_display: seat.wayland_display.clone(),
            env_file: seat.env_file.clone(),
            runtime_dir: seat.runtime_dir(),
            processes: seat.identities(),
        };
        table.seats.push(record);
        save(&table)?;
        seat.detach();
    }
    let status = inspect(table).await;
    if let Some(seat) = status
        .seats
        .iter()
        .find(|seat| seat.status.health != Health::Ready)
    {
        return Err(BackendError::Failed(format!(
            "Seat {} is not ready: {}",
            seat.seat.id,
            seat.status.issues.join("; ")
        )));
    }
    Ok(status)
}
pub fn down() -> Result<usize, BackendError> {
    let _lock = FileLock::acquire(&directory()?.join("ownership.lock"))?;
    let table = load()?;
    for seat in &table.seats {
        terminate_owned(&seat.processes, &seat.runtime_dir);
        let _ = std::fs::remove_dir_all(&seat.runtime_dir);
    }
    save(&FarmTable::default())?;
    Ok(table.seats.len())
}
pub async fn status() -> Result<FarmStatus, BackendError> {
    Ok(inspect(load()?).await)
}

async fn inspect(table: FarmTable) -> FarmStatus {
    let mut seats = Vec::new();
    for seat in table.seats {
        let status = seat.health().await;
        seats.push(SeatStatus { seat, status });
    }
    FarmStatus { seats }
}
