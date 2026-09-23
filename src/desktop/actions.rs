use super::Engine;
use crate::request::{Operation, Request};
use crate::{
    desktop::accessibility::{self as a11y, FlatElement},
    error::fail,
    platform::drivers::{Button, Ref, SessionType, TargetedInputDriver},
    platform::keymap::parse_chord,
    types::ToolError,
};
use serde_json::{Value, json};
use std::time::Duration;

/// Key hold for window-targeted chords: long enough for toolkits that
/// debounce, short enough to stay well inside auto-repeat delays.
const TARGETED_HOLD: Duration = Duration::from_millis(10);

/// Window-addressed delivery for one action, when the registry's targeted
/// driver can reach the app's current window.
type Route<'a> = Option<(&'a dyn TargetedInputDriver, Ref)>;

fn center(element: &FlatElement, missing: &str) -> Result<(i32, i32), ToolError> {
    element
        .bbox
        .filter(|bbox| bbox[2] > 0 && bbox[3] > 0)
        .map(|bbox| (bbox[0] + bbox[2] / 2, bbox[1] + bbox[3] / 2))
        .ok_or_else(|| fail("unsupported", missing))
}

fn path(start: (i32, i32), end: (i32, i32)) -> Vec<(i32, i32)> {
    (0..=24)
        .map(|step| {
            (
                start.0 + (end.0 - start.0) * step / 24,
                start.1 + (end.1 - start.1) * step / 24,
            )
        })
        .collect()
}

impl Engine {
    pub(super) async fn perform(&self, request: Request) -> Result<Value, ToolError> {
        let _guard = self.mutation.lock().await;
        let id = request.target()?;
        let result = match &request.operation {
            Operation::Click(args) => {
                let count = args.click_count;
                if !(1..=3).contains(&count) {
                    return Err(fail("invalid_argument", "clickCount must be 1..3"));
                }
                let button = args.mouse_button;
                if let Some(index) = args.target.element() {
                    let element = self.element(id, index).await?;
                    if button == Button::Left && count == 1 {
                        match a11y::target::action(&self.atspi, &element, None).await {
                            Ok(()) => {
                                self.invalidate(id).await;
                                return Ok(Value::Null);
                            }
                            Err(error) if error.code == "unsupported" => {}
                            Err(error) => return Err(error),
                        }
                    }
                    let missing = "Element has neither an action nor usable bounds";
                    if let Some((driver, target)) = self.route(id).await? {
                        let (x, y) = center(&element, missing)?;
                        driver.click(&target, x, y, button, count as u32).await
                    } else {
                        self.focus(id).await?;
                        let (x, y) = center(&self.element(id, index).await?, missing)?;
                        let input = self.registry.input()?;
                        self.position_pointer(id, x, y).await?;
                        self.element(id, index).await?;
                        for _ in 0..count {
                            input.click(x, y, button, vec![]).await?;
                        }
                        Ok(())
                    }
                } else if let Some((driver, target)) = self.route(id).await? {
                    let (x, y) = self.point(id, &args.target).await?;
                    driver.click(&target, x, y, button, count as u32).await
                } else {
                    self.focus(id).await?;
                    let (x, y) = self.point(id, &args.target).await?;
                    self.position_pointer(id, x, y).await?;
                    self.point(id, &args.target).await?;
                    for _ in 0..count {
                        self.registry.input()?.click(x, y, button, vec![]).await?;
                    }
                    Ok(())
                }
            }
            Operation::Drag { from, to } => {
                if let Some((driver, target)) = self.route(id).await? {
                    let start = self.point(id, from).await?;
                    let end = self.point(id, to).await?;
                    driver
                        .drag(&target, path(start, end), Button::Left, 300, 15)
                        .await
                } else {
                    self.focus(id).await?;
                    let start = self.point(id, from).await?;
                    let end = self.point(id, to).await?;
                    self.position_pointer(id, start.0, start.1).await?;
                    self.point(id, from).await?;
                    self.registry
                        .input()?
                        .drag(path(start, end), Button::Left, 300, 15)
                        .await
                }
            }
            Operation::Scroll(args) => {
                let route = self.route(id).await?;
                if route.is_none() {
                    self.focus(id).await?;
                }
                let (x, y) = if let Some(index) = args.target.element() {
                    center(
                        &self.element(id, index).await?,
                        "Element has no usable scroll bounds",
                    )?
                } else {
                    self.point(id, &args.target).await?
                };
                let pages = args.pages;
                if !pages.is_finite() || pages <= 0.0 || pages > 20.0 {
                    return Err(fail(
                        "invalid_argument",
                        "pages must be greater than zero and at most 20",
                    ));
                }
                let window = self.window(id).await?;
                let horizontal = args.direction.horizontal();
                let amount = (pages
                    * if horizontal {
                        window.geometry.w
                    } else {
                        window.geometry.h
                    } as f64)
                    .round() as i32;
                let (dx, dy) = args.direction.delta(amount as f64);
                if let Some((driver, target)) = route {
                    driver.scroll(&target, x, y, dx as i32, dy as i32).await
                } else {
                    self.focus(id).await?;
                    self.position_pointer(id, x, y).await?;
                    self.registry
                        .input()?
                        .scroll(x, y, dx as i32, dy as i32, vec![])
                        .await
                }
            }
            Operation::PressKey { key } => {
                let chord = parse_chord(key).map_err(|error| error.tool(false))?;
                if let Some((driver, target)) = self.route(id).await? {
                    driver.key(&target, vec![chord], TARGETED_HOLD).await
                } else {
                    self.focus(id).await?;
                    self.registry.input()?.key(vec![chord]).await
                }
            }
            Operation::TypeText { text } => {
                if text.len() > 100_000 {
                    return Err(fail("invalid_argument", "Text exceeds 100000 bytes"));
                }
                if text.is_empty() {
                    return Ok(Value::Null);
                }
                let typed = if let Some((driver, target)) = self.route(id).await? {
                    driver.type_text(&target, text.clone()).await
                } else {
                    self.focus(id).await?;
                    self.registry.input()?.type_text(text.clone()).await
                };
                match typed {
                    Err(error) if error.code == "unsupported" => self.paste(id, text).await,
                    result => result,
                }
            }
            Operation::Paste { text, format } => {
                if format != "text" {
                    return Err(fail(
                        "unsupported",
                        "Native rich clipboard formats are not available",
                    ));
                }
                self.paste(id, text).await
            }
            Operation::SetValue {
                element_index,
                value,
            } => {
                let element = self.element(id, *element_index).await?;
                a11y::target::set_value(&self.atspi, &element, value).await
            }
            Operation::SelectText(args) => {
                let element = self.element(id, args.element_index).await?;
                a11y::target::select_text(
                    &self.atspi,
                    &element,
                    &args.text,
                    &args.prefix,
                    &args.suffix,
                    &args.selection_type,
                )
                .await
            }
            Operation::PerformSecondaryAction {
                element_index,
                action,
            } => {
                let element = self.element(id, *element_index).await?;
                a11y::target::action(&self.atspi, &element, Some(action)).await
            }
            _ => Err(fail(
                "unsupported",
                "Operation is not supported by a desktop application",
            )),
        };
        self.invalidate(id).await;
        result.map(|()| json!(null))
    }

    /// Whether the app's current window takes window-addressed input. `None`
    /// means the foreground path: activate the window, move the pointer,
    /// then dispatch through the seat.
    async fn route(&self, id: &str) -> Result<Route<'_>, ToolError> {
        let Some(driver) = self.registry.targeted() else {
            return Ok(None);
        };
        let window = self.window(id).await?;
        Ok(driver
            .resolve(&window)
            .await?
            .map(|target| (driver, target)))
    }

    async fn paste(&self, id: &str, text: &str) -> Result<(), ToolError> {
        let chord = parse_chord("ctrl+v").map_err(|error| error.tool(false))?;
        let route = self.route(id).await?;
        if route.is_none() {
            self.focus(id).await?;
        }
        // A targeted window is an X11 client and pastes from the X11
        // selection, which the compositor only bridges from the Wayland
        // clipboard while an Xwayland window is active: serve the text there
        // directly, and the Wayland clipboard stays untouched while a native
        // window is active.
        let session = match route {
            Some(_) => SessionType::X11,
            None => self.session,
        };
        let clipboard = crate::desktop::clipboard::Clipboard::replace(session, text).await?;
        let input = match &route {
            Some((driver, target)) => driver.key(target, vec![chord], TARGETED_HOLD).await,
            None => self.registry.input()?.key(vec![chord]).await,
        };
        // Keep serving the selection until the target has had time to service
        // the paste; software-rendered sessions need well over the dispatch
        // latency to copy it, and restoring early loses the paste entirely.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let restore = clipboard.restore().await;
        input?;
        restore
    }
}
