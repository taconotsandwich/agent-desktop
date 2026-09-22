use super::events::Events;
use crate::error::BackendError;
use crate::platform::keyboard::NativeState;
use reis::{
    ei, enumflags2,
    event::{Connection, Device, DeviceCapability, EiEvent},
};
use std::sync::Mutex;
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
use tokio::sync::{mpsc, oneshot};

pub(super) struct Link {
    pub context: ei::Context,
    pub device: Device,
    pub keyboard_device: Option<Device>,
    pub connection: Connection,
    pub alive: Arc<AtomicBool>,
    pub stop: Option<oneshot::Sender<()>>,
    pub keyboard_map: Option<String>,
    pub keyboard_state: Arc<Mutex<NativeState>>,
    synchronize: mpsc::Sender<oneshot::Sender<()>>,
}

pub(super) async fn connect(
    raw_fd: i32,
    keyboard: bool,
    pointer: bool,
) -> Result<Link, BackendError> {
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let (sender, receiver) = oneshot::channel();
    let (stop, stopped) = oneshot::channel();
    let (synchronize, synchronization) = mpsc::channel(1);
    let keyboard_state = Arc::new(Mutex::new(NativeState::default()));
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
                let bootstrap = tokio::time::timeout(
                    Duration::from_secs(3),
                    initialize(fd, keyboard, pointer, &keyboard_state),
                )
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
                let keyboard_map = match read_keymap(keyboard_device.as_ref().unwrap_or(&device)) {
                    Ok(map) => map,
                    Err(error) => {
                        let _ = sender.send(Err(error));
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
                    keyboard_map,
                    keyboard_state: keyboard_state.clone(),
                    synchronize,
                };
                if sender.send(Ok(link)).is_ok() {
                    pump(
                        events,
                        stopped,
                        synchronization,
                        device,
                        keyboard_device,
                        alive,
                        keyboard_state,
                    )
                    .await;
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
    keyboard_state: &Mutex<NativeState>,
) -> Result<(ei::Context, Connection, Device, Option<Device>, Events), BackendError> {
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true).map_err(failed)?;
    let context = ei::Context::new(stream).map_err(failed)?;
    let mut events = Events::open(&context).await?;
    let connection = events.connection();
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
            EiEvent::KeyboardModifiers(event) => update_state(keyboard_state, event),
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
    mut events: Events,
    mut stopped: oneshot::Receiver<()>,
    mut synchronization: mpsc::Receiver<oneshot::Sender<()>>,
    device: Device,
    keyboard: Option<Device>,
    alive: Arc<AtomicBool>,
    keyboard_state: Arc<Mutex<NativeState>>,
) {
    loop {
        let event = tokio::select! {
            _=&mut stopped=>break,
            request=synchronization.recv()=> {
                let Some(done) = request else { break; };
                if events.synchronize(done).is_err() { break; }
                continue;
            },
            event=events.next()=>event,
        };
        let failed = match event {
            Some(Ok(EiEvent::KeyboardModifiers(event))) => {
                if event.device == device || keyboard.as_ref() == Some(&event.device) {
                    update_state(&keyboard_state, event);
                }
                false
            }
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

pub(super) fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::InputDispatchFailed {
        detail: error.to_string(),
    }
}

impl Link {
    pub async fn synchronize(&self) -> Result<(), BackendError> {
        let (sender, receiver) = oneshot::channel();
        self.synchronize.send(sender).await.map_err(failed)?;
        tokio::time::timeout(Duration::from_secs(3), receiver)
            .await
            .map_err(failed)?
            .map_err(failed)
    }
}

fn update_state(state: &Mutex<NativeState>, event: reis::event::KeyboardModifiers) {
    *state.lock().expect("keyboard state") = NativeState {
        depressed: event.depressed,
        latched: event.latched,
        locked: event.locked,
        group: event.group,
    };
}

fn read_keymap(device: &Device) -> Result<Option<String>, BackendError> {
    use std::os::unix::fs::FileExt;
    let Some(map) = device.keymap() else {
        return Ok(None);
    };
    if map.type_ != ei::keyboard::KeymapType::Xkb || map.size == 0 || map.size > 16 * 1024 * 1024 {
        return Err(failed("unsupported EIS keyboard map"));
    }
    let file = std::fs::File::from(map.fd.try_clone().map_err(failed)?);
    let mut bytes = vec![0; map.size as usize];
    file.read_exact_at(&mut bytes, 0).map_err(failed)?;
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    String::from_utf8(bytes).map(Some).map_err(failed)
}
