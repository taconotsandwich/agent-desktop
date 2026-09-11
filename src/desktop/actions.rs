use super::{Engine, Request, string, target};
use crate::{
    desktop::accessibility as a11y, error::fail, platform::drivers::Button, types::ToolError,
};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn perform(&self, request: Request) -> Result<Value, ToolError> {
        let _guard = self.mutation.lock().await;
        let id = target(&request)?;
        let args = &request.args;
        let result = match request.method.as_str() {
            "click" => {
                let count = args["clickCount"].as_u64().unwrap_or(1);
                if !(1..=3).contains(&count) {
                    return Err(fail("invalid_argument", "clickCount must be 1..3"));
                }
                let button = match args["mouseButton"].as_str().unwrap_or("left") {
                    "left" | "l" => Button::Left,
                    "right" | "r" => Button::Right,
                    "middle" | "m" => Button::Middle,
                    _ => return Err(fail("invalid_argument", "Unknown mouseButton")),
                };
                if let Some(index) = args["target"].as_u64() {
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
                    let (x, y) = self.point(id, &args["target"]).await?;
                    self.position_pointer(id, x, y).await?;
                    self.point(id, &args["target"]).await?;
                    for _ in 0..count {
                        self.registry.input()?.click(x, y, button, vec![]).await?;
                    }
                    Ok(())
                }
            }
            "drag" => {
                self.focus(id).await?;
                let from = self.point(id, &args["from"]).await?;
                let to = self.point(id, &args["to"]).await?;
                self.position_pointer(id, from.0, from.1).await?;
                self.point(id, &args["from"]).await?;
                let path = (0..=24)
                    .map(|step| {
                        (
                            from.0 + (to.0 - from.0) * step / 24,
                            from.1 + (to.1 - from.1) * step / 24,
                        )
                    })
                    .collect();
                self.registry
                    .input()?
                    .drag(path, Button::Left, 300, 15)
                    .await
            }
            "scroll" => {
                self.focus(id).await?;
                let (x, y) = if let Some(index) = args["target"].as_u64() {
                    let element = self.element(id, index).await?;
                    let bbox = element
                        .bbox
                        .filter(|bbox| bbox[2] > 0 && bbox[3] > 0)
                        .ok_or_else(|| {
                            fail("unsupported", "Element has no usable scroll bounds")
                        })?;
                    (bbox[0] + bbox[2] / 2, bbox[1] + bbox[3] / 2)
                } else {
                    self.point(id, &args["target"]).await?
                };
                let pages = args["pages"].as_f64().unwrap_or(1.0);
                if !pages.is_finite() || pages <= 0.0 || pages > 20.0 {
                    return Err(fail(
                        "invalid_argument",
                        "pages must be greater than zero and at most 20",
                    ));
                }
                let window = self.window(id).await?;
                let horizontal = matches!(
                    args["direction"].as_str(),
                    Some("left" | "right" | "l" | "r")
                );
                let amount = (pages
                    * if horizontal {
                        window.geometry.w
                    } else {
                        window.geometry.h
                    } as f64)
                    .round() as i32;
                let (dx, dy) = match string(args, "direction")? {
                    "up" | "u" => (0, -amount),
                    "down" | "d" => (0, amount),
                    "left" | "l" => (-amount, 0),
                    "right" | "r" => (amount, 0),
                    _ => return Err(fail("invalid_argument", "Unknown scroll direction")),
                };
                self.focus(id).await?;
                self.position_pointer(id, x, y).await?;
                self.registry.input()?.scroll(x, y, dx, dy, vec![]).await
            }
            "pressKey" => {
                let key = string(args, "key")?;
                crate::platform::keymap::parse_chord(key).map_err(|error| error.tool(false))?;
                self.focus(id).await?;
                self.registry.input()?.key(vec![key.into()]).await
            }
            "typeText" => {
                let text = string(args, "text")?;
                if text.len() > 100_000 {
                    return Err(fail("invalid_argument", "Text exceeds 100000 bytes"));
                }
                if text.is_empty() {
                    return Ok(Value::Null);
                }
                self.focus(id).await?;
                match self.registry.input()?.type_text(text.into()).await {
                    Err(error) if error.code == "unsupported" => self.paste(id, text).await,
                    result => result,
                }
            }
            "paste" => {
                let format = args["format"].as_str().unwrap_or("text");
                if format != "text" {
                    return Err(fail(
                        "unsupported",
                        "Native rich clipboard formats are not available",
                    ));
                }
                self.paste(id, string(args, "text")?).await
            }
            "setValue" => {
                let index = args["elementIndex"]
                    .as_u64()
                    .ok_or_else(|| fail("invalid_argument", "elementIndex is required"))?;
                let element = self.element(id, index).await?;
                a11y::target::set_value(&self.atspi, &element, string(args, "value")?).await
            }
            "selectText" => {
                let index = args["elementIndex"]
                    .as_u64()
                    .ok_or_else(|| fail("invalid_argument", "elementIndex is required"))?;
                let element = self.element(id, index).await?;
                a11y::target::select_text(
                    &self.atspi,
                    &element,
                    string(args, "text")?,
                    args["prefix"].as_str().unwrap_or(""),
                    args["suffix"].as_str().unwrap_or(""),
                    args["selectionType"].as_str().unwrap_or("text"),
                )
                .await
            }
            "performSecondaryAction" => {
                let index = args["elementIndex"]
                    .as_u64()
                    .ok_or_else(|| fail("invalid_argument", "elementIndex is required"))?;
                let element = self.element(id, index).await?;
                a11y::target::action(&self.atspi, &element, Some(string(args, "action")?)).await
            }
            _ => Err(fail(
                "unsupported",
                format!("Unknown operation {}", request.method),
            )),
        };
        self.invalidate(id).await;
        result.map(|()| json!(null))
    }

    async fn paste(&self, id: &str, text: &str) -> Result<(), ToolError> {
        self.focus(id).await?;
        let clipboard = crate::desktop::clipboard::Clipboard::replace(self.session, text).await?;
        let input = self.registry.input()?.key(vec!["ctrl+v".into()]).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let restore = clipboard.restore().await;
        input?;
        restore
    }
}
