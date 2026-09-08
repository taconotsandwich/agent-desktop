//! Seat farm: N parallel virtual seats behind one command.
//!
//! Each seat is a full isolated compositor (own bus, socket, AT-SPI) — the
//! unit of Codex-style parallel background agents. The farm boots seats,
//! records them in a table file, and tears them down. Task routing across
//! seats belongs to the orchestrating client; each seat is driven through
//! its own `agent-desktop --mode=virtual --join-seat <env>` server (stdio).

use crate::error::BackendError;
use crate::mode::{SeatParams, VirtualSeat};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatRecord {
    pub id: u32,
    pub pids: Vec<i32>,
    pub bus_address: String,
    pub wayland_display: String,
    pub env_file: String,
    pub vnc_port: u16,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FarmTable {
    pub seats: Vec<SeatRecord>,
}

pub fn farm_file() -> String {
    std::env::var("AGENT_DESKTOP_FARM_FILE")
        .unwrap_or_else(|_| "/tmp/agent-desktop-farm.json".into())
}

fn load_table() -> FarmTable {
    std::fs::read_to_string(farm_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_table(table: &FarmTable) -> Result<(), BackendError> {
    let s = serde_json::to_string_pretty(table).map_err(|e| BackendError::Failed(e.to_string()))?;
    std::fs::write(farm_file(), s).map_err(|e| BackendError::Io {
        path: farm_file(),
        error: e.to_string(),
    })
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Boot `n` seats (ids 0..n) sequentially. KWin boots are slow (~8s each);
/// a 1920x1080 software seat costs roughly 300-500MB RAM — size farms to
/// the host (Blender-capable seats want llvmpipe CPU too).
pub async fn up(n: u32, width: u32, height: u32) -> Result<FarmTable, BackendError> {
    if n == 0 || n > 16 {
        return Err(BackendError::Unsupported {
            reason: "farm size must be 1..=16".into(),
        });
    }
    let mut table = load_table();
    // Drop records whose seats are already gone (stale from killed farms).
    table.seats.retain(|s| s.pids.iter().any(|p| alive(*p)));
    for id in 0..n {
        if table.seats.iter().any(|s| s.id == id && s.pids.iter().any(|p| alive(*p))) {
            continue;
        }
        table.seats.retain(|s| s.id != id);
        let seat = VirtualSeat::boot_with(SeatParams { id, width, height }).await?;
        // Reap the VirtualSeat handle without killing: farm owns lifetimes
        // via the table, not via the handle.
        let record = SeatRecord {
            id,
            bus_address: seat.bus_address.clone(),
            wayland_display: seat.wayland_display.clone(),
            env_file: seat.env_file.clone(),
            vnc_port: SeatParams { id, width, height }.vnc_port(),
            pids: seat.into_pids(),
        };
        table.seats.push(record);
        save_table(&table)?;
        tracing::info!(seat = id, "farm seat up");
    }
    table.seats.sort_by_key(|s| s.id);
    save_table(&table)?;
    Ok(table)
}

/// SIGTERM/SIGKILL every tracked pid, remove env files + table.
pub fn down() -> Result<FarmTable, BackendError> {
    let table = load_table();
    for seat in &table.seats {
        for pid in &seat.pids {
            unsafe {
                libc::kill(*pid, libc::SIGTERM);
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    for seat in &table.seats {
        for pid in &seat.pids {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&seat.env_file);
    }
    let _ = std::fs::remove_file(farm_file());
    Ok(table)
}

pub fn status() -> FarmTable {
    load_table()
}
