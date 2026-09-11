use crate::platform::wayland::eis::EisConnector;
use crate::{error::BackendError, platform::drivers::Probe};
use zbus::Connection;

#[zbus::proxy(
    interface = "org.kde.KWin.EIS.RemoteDesktop",
    default_service = "org.kde.KWin",
    default_path = "/org/kde/KWin/EIS/RemoteDesktop"
)]
trait RemoteDesktop {
    #[zbus(name = "connectToEIS")]
    fn connect_to_eis(&self, flags: i32) -> zbus::Result<(zvariant::OwnedFd, i32)>;
}
pub struct KwinConnector {
    bus: Connection,
}
impl KwinConnector {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }
}
#[async_trait::async_trait]
impl EisConnector for KwinConnector {
    fn id(&self) -> &'static str {
        "kwin-eis"
    }
    async fn probe(&self) -> Probe {
        super::kwin_probe(&self.bus, self.id()).await
    }
    async fn connect(&self) -> Result<std::os::fd::OwnedFd, BackendError> {
        let proxy = RemoteDesktopProxy::new(&self.bus).await.map_err(|error| {
            BackendError::BusDisconnected {
                detail: error.to_string(),
            }
        })?;
        let (fd, _) =
            proxy
                .connect_to_eis(3)
                .await
                .map_err(|error| BackendError::BusDisconnected {
                    detail: error.to_string(),
                })?;
        Ok(fd.into())
    }
}
