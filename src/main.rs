use agent_desktop::{
    a11y::AtspiConnection,
    backends::{KwinInput, KwinShot, KwinWindows, detect_desktop, detect_session},
    core::RefStore,
    drivers::{Desktop, SessionType},
    registry::Registry,
    server::AgentDesktop,
};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "agent-desktop", about = "General Linux desktop-control MCP server")]
struct Cli {
    /// live | virtual (virtual spawns an isolated compositor; KDE first)
    #[arg(long, default_value = "live")]
    mode: String,
    /// kwin | gnome | x11 | fake — default: auto-detect
    #[arg(long)]
    backend: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let session = detect_session();
    let desktop = detect_desktop();
    tracing::info!(mode = %cli.mode, ?session, ?desktop, "agent-desktop starting (stdio)");

    let bus = zbus::Connection::session().await?;
    let atspi = Arc::new(AtspiConnection::new());
    let server = AgentDesktop::new(Arc::new(RefStore::new()), bus.clone(), atspi, session);

    // Build + probe drivers. Override order follows BACKEND env or detection.
    let forced = cli.backend.as_deref();
    let use_kwin = forced.map(|b| b == "kwin").unwrap_or(matches!(
        (session, desktop),
        (SessionType::Wayland, Desktop::Kde)
    ));
    if use_kwin {
        let reg = Registry::probe(
            vec![Arc::new(KwinShot::new(bus.clone()))],
            vec![Arc::new(KwinInput::new(bus.clone()))],
            vec![Arc::new(KwinWindows::new(bus.clone()))],
        )
        .await;
        match reg {
            Ok(r) => {
                tracing::info!(
                    shot = r.shot.id(),
                    input = r.input.id(),
                    windows = r.windows.id(),
                    "backends probed ok"
                );
                *server.registry.write().await = Some(r);
            }
            Err(probes) => {
                for p in &probes {
                    tracing::warn!(id = p.id, ok = p.ok, detail = %p.detail, "probe");
                }
                tracing::warn!("no full backend set; doctor reports blockers");
            }
        }
    } else {
        tracing::warn!("no drivers for this session/desktop yet (gnome/x11 land next)");
    }

    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}
