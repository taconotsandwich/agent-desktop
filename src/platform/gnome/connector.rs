use crate::platform::wayland::eis::EisConnector;
use crate::{error::BackendError, platform::drivers::Probe};
use std::collections::HashMap;
use tokio::sync::Mutex;
use zbus::{Connection, proxy::Proxy};
use zvariant::{OwnedObjectPath, Value};

const SERVICE: &str = "org.gnome.Mutter.RemoteDesktop";
const INTERFACE: &str = "org.gnome.Mutter.RemoteDesktop.Session";

pub struct MutterConnector {
    bus: Connection,
    session: Mutex<Option<Session>>,
}
struct Session {
    bus: Connection,
    path: OwnedObjectPath,
}
impl MutterConnector {
    pub fn new(bus: Connection) -> Self {
        Self {
            bus,
            session: Mutex::new(None),
        }
    }
}
impl Session {
    async fn create() -> Result<Self, BackendError> {
        let bus = Connection::session().await.map_err(failed)?;
        let proxy = Proxy::new(&bus, SERVICE, "/org/gnome/Mutter/RemoteDesktop", SERVICE)
            .await
            .map_err(failed)?;
        let path: OwnedObjectPath = proxy.call("CreateSession", &()).await.map_err(failed)?;
        drop(proxy);
        let session = Self { bus, path };
        session
            .proxy()
            .await?
            .call::<_, _, ()>("Start", &())
            .await
            .map_err(failed)?;
        Ok(session)
    }
    async fn proxy(&self) -> Result<Proxy<'_>, BackendError> {
        Proxy::new(&self.bus, SERVICE, self.path.as_str(), INTERFACE)
            .await
            .map_err(failed)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let bus = self.bus.clone();
        let path = self.path.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let stop = async {
                    if let Ok(proxy) = Proxy::new(&bus, SERVICE, path, INTERFACE).await {
                        let _: Result<(), _> = proxy.call("Stop", &()).await;
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), stop).await;
                let _ = bus.close().await;
            });
        }
    }
}
#[async_trait::async_trait]
impl EisConnector for MutterConnector {
    fn id(&self) -> &'static str {
        "mutter-eis"
    }
    async fn probe(&self) -> Probe {
        let result = async {
            let proxy = Proxy::new(
                &self.bus,
                SERVICE,
                "/org/gnome/Mutter/RemoteDesktop",
                SERVICE,
            )
            .await?;
            proxy.get_property::<i32>("Version").await
        }
        .await;
        Probe {
            id: self.id(),
            ok: result.is_ok(),
            detail: result
                .map(|version| format!("Mutter RemoteDesktop version {version}"))
                .unwrap_or_else(|error| error.to_string()),
        }
    }
    async fn connect(&self) -> Result<std::os::fd::OwnedFd, BackendError> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = Some(Session::create().await?);
        }
        let result = async {
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            let fd: zvariant::OwnedFd = session
                .as_ref()
                .expect("created session")
                .proxy()
                .await?
                .call("ConnectToEIS", &(options,))
                .await
                .map_err(failed)?;
            Ok(fd.into())
        }
        .await;
        if result.is_err() {
            session.take();
        }
        result
    }
}
fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::InputDispatchFailed {
        detail: error.to_string(),
    }
}
