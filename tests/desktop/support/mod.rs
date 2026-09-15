pub mod blender;
use agent_desktop::session::seat::{SeatParams, VirtualSeat};
use std::{collections::BTreeMap, path::PathBuf};
#[path = "../../support/binary.rs"]
mod binary;
pub mod mcp;
pub use binary::server_bin;

pub struct Seat {
    pub owned: Option<VirtualSeat>,
    pub env_file: String,
    pub environment: BTreeMap<String, String>,
    pub artifacts: PathBuf,
}

impl Seat {
    pub async fn boot(name: &str) -> anyhow::Result<Self> {
        let artifacts = std::env::var_os("AGENT_DESKTOP_QA_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/qa"))
            .join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&artifacts)?;
        let owned = if std::env::var_os("AGENT_DESKTOP_QA_SESSION").is_none() {
            Some(
                VirtualSeat::boot_with(SeatParams {
                    id: 51,
                    width: 1800,
                    height: 1125,
                })
                .await?,
            )
        } else {
            None
        };
        let env_file = std::env::var("AGENT_DESKTOP_QA_SESSION")
            .unwrap_or_else(|_| owned.as_ref().unwrap().env_file.clone());
        let environment = agent_desktop::session::environment::read_environment(&env_file)?;
        if let Some(seat) = &owned {
            for file in [
                "kwin_wayland.log",
                "at-spi-bus-launcher.log",
                "at-spi2-registryd.log",
            ] {
                let _ = std::fs::copy(
                    PathBuf::from(seat.runtime_dir()).join(file),
                    artifacts.join(file),
                );
            }
        }
        Ok(Self {
            owned,
            env_file,
            environment,
            artifacts,
        })
    }
}
impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(seat) = &self.owned
            && let Ok(entries) = std::fs::read_dir(seat.runtime_dir())
        {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "log") {
                    let _ = std::fs::copy(entry.path(), self.artifacts.join(entry.file_name()));
                }
            }
        }
    }
}
