use super::{PortalSession, connect_eis_fd, ensure_session, portal_present};
use crate::platform::wayland::eis::EisConnector;
use crate::{error::BackendError, platform::drivers::Probe};
use tokio::sync::Mutex;
use zbus::Connection;
pub struct PortalConnector {
    bus: Connection,
    session: Mutex<Option<PortalSession>>,
}

impl PortalConnector {
    pub fn new(bus: Connection) -> Self {
        Self {
            bus,
            session: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl EisConnector for PortalConnector {
    fn id(&self) -> &'static str {
        "portal-remotedesktop"
    }
    async fn probe(&self) -> Probe {
        let ok = portal_present(&self.bus).await;
        Probe {
            id: self.id(),
            ok,
            detail: if ok {
                "Portal present; authorization is requested on first input".into()
            } else {
                "RemoteDesktop portal unavailable".into()
            },
        }
    }
    async fn connect(&self) -> Result<std::os::fd::OwnedFd, BackendError> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = Some(ensure_session(&self.bus).await?);
        }
        let result = connect_eis_fd(
            &self.bus,
            &session.as_ref().expect("session initialized").path,
        )
        .await;
        if result.is_err() {
            session.take();
        }
        result
    }
}
