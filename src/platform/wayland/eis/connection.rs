use crate::error::BackendError;
use futures_util::StreamExt;
use reis::{
    ei, enumflags2,
    event::{Connection, Device, DeviceCapability, EiEvent},
};
use std::{
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

pub(super) struct Link {
    pub context: ei::Context,
    pub device: Device,
    pub keyboard_device: Option<Device>,
    pub connection: Connection,
    pub alive: Arc<AtomicBool>,
    pub stop: Option<oneshot::Sender<()>>,
}

pub(super) async fn connect(
    raw_fd: i32,
    keyboard: bool,
    pointer: bool,
) -> Result<Link, BackendError> {
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let (sender, receiver) = oneshot::channel();
    let (stop, stopped) = oneshot::channel();
    std::thread::Builder::new()
        .name("desktop-eis".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sender.send(Err(failed(error)));
                    return;
                }
            };
            runtime.block_on(async move {
                let bootstrap =
                    tokio::time::timeout(Duration::from_secs(3), initialize(fd, keyboard, pointer))
                        .await;
                let (context, connection, device, keyboard_device, events) = match bootstrap {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                    Err(_) => {
                        let _ =
                            sender.send(Err(failed("EIS devices did not resume within 3 seconds")));
                        return;
                    }
                };
                let alive = Arc::new(AtomicBool::new(true));
                let link = Link {
                    context,
                    connection,
                    device: device.clone(),
                    keyboard_device: keyboard_device.clone(),
                    alive: alive.clone(),
                    stop: Some(stop),
                };
                if sender.send(Ok(link)).is_ok() {
                    pump(events, stopped, device, keyboard_device, alive).await;
                }
            });
        })
        .map_err(failed)?;
    receiver.await.map_err(failed)?
}

async fn initialize(
    fd: OwnedFd,
    keyboard: bool,
    pointer: bool,
) -> Result<
    (
        ei::Context,
        Connection,
        Device,
        Option<Device>,
        reis::tokio::EiConvertEventStream,
    ),
    BackendError,
> {
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true).map_err(failed)?;
    let context = ei::Context::new(stream).map_err(failed)?;
    let (connection, mut events) = context
        .handshake_tokio("agent-desktop", ei::handshake::ContextType::Sender)
        .await
        .map_err(failed)?;
    let mut chosen: Option<Device> = None;
    let mut auxiliary: Option<Device> = None;
    let mut resumed = Vec::new();
    while let Some(event) = events.next().await {
        match event.map_err(failed)? {
            EiEvent::SeatAdded(event) => {
                let mut capabilities = enumflags2::BitFlags::<DeviceCapability>::empty();
                if keyboard {
                    capabilities |= DeviceCapability::Keyboard;
                }
                if pointer {
                    capabilities |= DeviceCapability::Pointer
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll;
                }
                event.seat.bind_capabilities(capabilities);
                context.flush().map_err(failed)?;
            }
            EiEvent::DeviceAdded(event) => {
                let has_pointer = event
                    .device
                    .has_capability(DeviceCapability::PointerAbsolute)
                    && event.device.has_capability(DeviceCapability::Button);
                let has_keyboard = event.device.has_capability(DeviceCapability::Keyboard);
                if pointer && has_pointer && chosen.is_none() {
                    chosen = Some(event.device.clone());
                }
                if keyboard && has_keyboard {
                    if pointer {
                        auxiliary = Some(event.device);
                    } else {
                        chosen = Some(event.device);
                    }
                }
            }
            EiEvent::DeviceResumed(event) => resumed.push(event.device),
            EiEvent::DevicePaused(event) => resumed.retain(|device| device != &event.device),
            EiEvent::Disconnected(event) => {
                return Err(failed(format!("EIS disconnected: {:?}", event.explanation)));
            }
            _ => {}
        }
        let primary_ready = chosen
            .as_ref()
            .is_some_and(|device| resumed.contains(device));
        let auxiliary_ready = !(keyboard && pointer)
            || auxiliary
                .as_ref()
                .is_some_and(|device| resumed.contains(device));
        if primary_ready && auxiliary_ready {
            return Ok((
                context,
                connection,
                chosen.expect("resumed primary"),
                auxiliary,
                events,
            ));
        }
    }
    Err(failed("EIS connection ended during setup"))
}

async fn pump(
    mut events: reis::tokio::EiConvertEventStream,
    mut stopped: oneshot::Receiver<()>,
    device: Device,
    keyboard: Option<Device>,
    alive: Arc<AtomicBool>,
) {
    loop {
        let event = tokio::select! {
            _=&mut stopped=>break,
            event=events.next()=>event,
        };
        let failed = match event {
            Some(Ok(EiEvent::DevicePaused(event))) => {
                event.device == device || keyboard.as_ref() == Some(&event.device)
            }
            Some(Ok(EiEvent::DeviceRemoved(event))) => {
                event.device == device || keyboard.as_ref() == Some(&event.device)
            }
            Some(Ok(EiEvent::Disconnected(_))) | Some(Err(_)) | None => true,
            _ => false,
        };
        if failed {
            break;
        }
    }
    alive.store(false, Ordering::Release);
}

fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::InputDispatchFailed {
        detail: error.to_string(),
    }
}
