use agent_desktop::{
    desktop::{Engine, accessibility::AtspiConnection},
    mcp::AgentDesktop,
    platform::{
        detect_desktop, detect_session,
        drivers::{Desktop, InputDriver, SessionType, ShotDriver, WindowDriver},
        gnome::{GnomeShot, GnomeWindows, MutterConnector},
        kde::{KwinConnector, KwinShot, KwinWindows},
        registry::Registry,
        wayland::{
            eis::EisInput,
            portal::{PortalConnector, PortalShot},
        },
        x11::{X11Input, X11Shot, X11Windows},
    },
    runtime::Javascript,
};
use rmcp::{ServiceExt, transport::stdio};
use std::sync::Arc;

pub(super) async fn serve(
    session: Option<SessionType>,
    desktop: Option<Desktop>,
    private_session: bool,
) -> anyhow::Result<()> {
    let session = session.unwrap_or_else(detect_session);
    let desktop = desktop.unwrap_or_else(detect_desktop);
    let bus = zbus::Connection::session().await?;
    let mut shots: Vec<Arc<dyn ShotDriver>> = Vec::new();
    let mut inputs: Vec<Arc<dyn InputDriver>> = Vec::new();
    let mut windows: Vec<Arc<dyn WindowDriver>> = Vec::new();
    match session {
        SessionType::X11 => {
            shots.push(Arc::new(X11Shot));
            inputs.push(Arc::new(X11Input));
            windows.push(Arc::new(X11Windows));
        }
        SessionType::Wayland => {
            match desktop {
                Desktop::Kde => {
                    if private_session {
                        agent_desktop::platform::kde::authorize_private_session().await?;
                    }
                    shots.push(Arc::new(KwinShot::new(bus.clone())));
                    inputs.push(Arc::new(EisInput::new(KwinConnector::new(bus.clone()))));
                    windows.push(Arc::new(KwinWindows::new(bus.clone())));
                }
                Desktop::Gnome => {
                    inputs.push(Arc::new(EisInput::new(MutterConnector::new(bus.clone()))));
                    windows.push(Arc::new(GnomeWindows::new(bus.clone())));
                    shots.push(Arc::new(GnomeShot::new(bus.clone())));
                }
                Desktop::Other => {}
            }
            shots.push(Arc::new(PortalShot::new(bus.clone())));
            inputs.push(Arc::new(EisInput::new(PortalConnector::new(bus))));
        }
    }
    let registry = Registry::probe(shots, inputs, windows).await;
    for probe in &registry.probes {
        tracing::info!(id=probe.id,ok=probe.ok,detail=%probe.detail,"backend probe");
    }
    let engine = Arc::new(Engine::new(
        registry,
        session,
        Arc::new(AtspiConnection::new()),
    ));
    let javascript = Javascript::new(engine);
    let server = AgentDesktop::new(javascript.clone());
    let result = server.serve(stdio()).await?.waiting().await;
    javascript.shutdown().await;
    result?;
    Ok(())
}
