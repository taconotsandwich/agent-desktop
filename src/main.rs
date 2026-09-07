use agent_desktop::{
    a11y::AtspiConnection,
    backends::{
        GnomeWindows, KwinInput, KwinShot, KwinWindows, PortalInput, PortalShot, X11Input, X11Shot,
        X11Windows, detect_desktop, detect_session,
    },
    core::RefStore,
    drivers::{Desktop, InputDriver, SessionType, ShotDriver, WindowDriver},
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
    // Registry order per capability: native first, generic X11 last.
    let forced = cli.backend.as_deref();
    let mut shots: Vec<Arc<dyn ShotDriver>> = Vec::new();
    let mut inputs: Vec<Arc<dyn InputDriver>> = Vec::new();
    let mut windows: Vec<Arc<dyn WindowDriver>> = Vec::new();
    let want_kwin = forced.map(|b| b == "kwin").unwrap_or(matches!(
        (session, desktop),
        (SessionType::Wayland, Desktop::Kde)
    ));
    let want_gnome = forced.map(|b| b == "gnome").unwrap_or(matches!(
        (session, desktop),
        (SessionType::Wayland, Desktop::Gnome)
    ));
    let want_x11 = forced.map(|b| b == "x11").unwrap_or(matches!(
        session,
        SessionType::X11
    ));
    if want_kwin {
        shots.push(Arc::new(KwinShot::new(bus.clone())));
        inputs.push(Arc::new(KwinInput::new(bus.clone())));
        windows.push(Arc::new(KwinWindows::new(bus.clone())));
    }
    if want_gnome {
        shots.push(Arc::new(PortalShot::new(bus.clone())));
        inputs.push(Arc::new(PortalInput::new(bus.clone())));
        windows.push(Arc::new(GnomeWindows::new(bus.clone())));
    }
    if want_x11 {
        shots.push(Arc::new(X11Shot));
        inputs.push(Arc::new(X11Input));
        windows.push(Arc::new(X11Windows));
    }
    // Portal is also a valid fallback on KDE Wayland when EIS is locked down.
    if want_kwin && forced.is_none() {
        shots.push(Arc::new(PortalShot::new(bus.clone())));
        inputs.push(Arc::new(PortalInput::new(bus.clone())));
    }
    if shots.is_empty() {
        tracing::warn!("no drivers for this session/desktop (use --backend to force)");
    } else {
        match Registry::probe(shots, inputs, windows).await {
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
    }

    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}
