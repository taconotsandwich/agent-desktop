use crate::error::BackendError;
use futures_util::StreamExt;
use reis::{
    ei,
    event::{EiEvent, EiEventConverter},
};
use tokio::sync::oneshot;

pub(super) struct Events {
    raw: reis::tokio::EiEventStream,
    converter: EiEventConverter,
}
impl Events {
    pub async fn open(context: &ei::Context) -> Result<Self, BackendError> {
        let mut raw =
            reis::tokio::EiEventStream::new(context.clone()).map_err(super::connection::failed)?;
        let handshake = reis::tokio::ei_handshake(
            &mut raw,
            "agent-desktop",
            ei::handshake::ContextType::Sender,
        )
        .await
        .map_err(super::connection::failed)?;
        Ok(Self {
            raw,
            converter: EiEventConverter::new(context, handshake),
        })
    }
    pub fn connection(&self) -> reis::event::Connection {
        self.converter.connection().clone()
    }
    pub fn synchronize(&mut self, done: oneshot::Sender<()>) -> Result<(), BackendError> {
        let connection = self.converter.connection();
        let callback = connection.connection().sync(1);
        self.converter.add_callback_handler(callback, |_| {
            let _ = done.send(());
        });
        self.converter
            .connection()
            .flush()
            .map_err(super::connection::failed)
    }
    pub async fn next(&mut self) -> Option<Result<EiEvent, BackendError>> {
        loop {
            if let Some(event) = self.converter.next_event() {
                return Some(Ok(event));
            }
            let request = match self.raw.next().await? {
                Ok(request) => request,
                Err(error) => return Some(Err(super::connection::failed(error))),
            };
            match request {
                reis::PendingRequestResult::Request(event) => {
                    if let Err(error) = self.converter.handle_event(event) {
                        return Some(Err(super::connection::failed(error)));
                    }
                }
                reis::PendingRequestResult::ParseError(error) => {
                    return Some(Err(super::connection::failed(error)));
                }
                reis::PendingRequestResult::InvalidObject(_) => {}
            }
        }
    }
}
