use super::Engine;
use crate::request::{Operation, Request};
use crate::{
    desktop::accessibility as a11y, error::fail, platform::drivers::Button, types::ToolError,
};
use serde_json::{Value, json};

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
                    self.focus(id).await?;
                    let element = self.element(id, index).await?;
                    let bbox = element
                        .bbox
                        .filter(|bbox| bbox[2] > 0 && bbox[3] > 0)
                        .ok_or_else(|| {
                            fail(
                                "unsupported",
                                "Element has neither an action nor usable bounds",
                            )
                        })?;
                    let input = self.registry.input()?;
                    self.position_pointer(id, bbox[0] + bbox[2] / 2, bbox[1] + bbox[3] / 2)
                        .await?;
                    self.element(id, index).await?;
                    for _ in 0..count {
                        input
                            .click(bbox[0] + bbox[2] / 2, bbox[1] + bbox[3] / 2, button, vec![])
                            .await?;
                    }
                    Ok(())
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
                self.focus(id).await?;
                let start = self.point(id, from).await?;
                let end = self.point(id, to).await?;
                self.position_pointer(id, start.0, start.1).await?;
                self.point(id, from).await?;
                let path = (0..=24)
                    .map(|step| {
                        (
                            start.0 + (end.0 - start.0) * step / 24,
                            start.1 + (end.1 - start.1) * step / 24,
                        )
                    })
                    .collect();
                self.registry
                    .input()?
                    .drag(path, Button::Left, 300, 15)
                    .await
            }
            Operation::Scroll(args) => {
                self.focus(id).await?;
                let (x, y) = if let Some(index) = args.target.element() {
                    let element = self.element(id, index).await?;
                    let bbox = element
                        .bbox
                        .filter(|bbox| bbox[2] > 0 && bbox[3] > 0)
                        .ok_or_else(|| {
                            fail("unsupported", "Element has no usable scroll bounds")
                        })?;
                    (bbox[0] + bbox[2] / 2, bbox[1] + bbox[3] / 2)
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
                self.focus(id).await?;
                self.position_pointer(id, x, y).await?;
                self.registry
                    .input()?
                    .scroll(x, y, dx as i32, dy as i32, vec![])
                    .await
            }
            Operation::PressKey { key } => {
                crate::platform::keymap::parse_chord(key).map_err(|error| error.tool(false))?;
                self.focus(id).await?;
                self.registry.input()?.key(vec![key.clone()]).await
            }
            Operation::TypeText { text } => {
                if text.len() > 100_000 {
                    return Err(fail("invalid_argument", "Text exceeds 100000 bytes"));
                }
                if text.is_empty() {
                    return Ok(Value::Null);
                }
                self.focus(id).await?;
                match self.registry.input()?.type_text(text.clone()).await {
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

    async fn paste(&self, id: &str, text: &str) -> Result<(), ToolError> {
        self.focus(id).await?;
        let clipboard = crate::desktop::clipboard::Clipboard::replace(self.session, text).await?;
        let input = self.registry.input()?.key(vec!["ctrl+v".into()]).await;
        // Keep serving the selection until the target has had time to service
        // the paste; software-rendered sessions need well over the dispatch
        // latency to copy it, and restoring early loses the paste entirely.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let restore = clipboard.restore().await;
        input?;
        restore
    }
}
