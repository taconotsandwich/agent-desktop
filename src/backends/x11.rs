//! X11 drivers — the generic last resort in every registry order.
//!
//! `maim`/`import` screenshots, `xdotool` input, `wmctrl` windows. All
//! subprocess-based, all requires `$DISPLAY`. One file: X11 erases
//! compositor differences, so KDE-X11 and GNOME-X11 share this.

use crate::drivers::{Bbox, Button, InputDriver, Probe, Ref, Shot, ShotDriver, ShotFormat, ShotTarget, WindowDriver, WindowInfo};
use crate::error::BackendError;
use crate::keymap;
use crate::types::ToolError;
use tokio::process::Command;

fn display() -> Result<String, BackendError> {
    std::env::var("DISPLAY").map_err(|_| BackendError::Unavailable {
        backend: "x11",
        detail: "DISPLAY not set".into(),
    })
}

fn has(bin: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join(bin).is_file())
}

async fn run(
    what: &'static str,
    cmd: &str,
    args: &[&str],
    display: &str,
) -> Result<Vec<u8>, BackendError> {
    let out = Command::new(cmd)
        .args(args)
        .env("DISPLAY", display)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| BackendError::Io {
            path: cmd.into(),
            error: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("{what}: {} exited {}", cmd, out.status),
        });
    }
    Ok(out.stdout)
}

// ---- Shot ----

pub struct X11Shot;

#[async_trait::async_trait]
impl ShotDriver for X11Shot {
    fn id(&self) -> &'static str {
        "x11-maim"
    }
    async fn probe(&self) -> Probe {
        match display() {
            Err(e) => Probe {
                id: self.id(),
                ok: false,
                detail: e.tool(false).message,
            },
            Ok(_) if has("maim") => Probe {
                id: self.id(),
                ok: true,
                detail: "maim on PATH".into(),
            },
            Ok(_) if has("import") => Probe {
                id: "x11-import",
                ok: true,
                detail: "import (ImageMagick) on PATH".into(),
            },
            Ok(_) => Probe {
                id: self.id(),
                ok: false,
                detail: "neither maim nor import found".into(),
            },
        }
    }
    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        if !matches!(target, ShotTarget::Full) {
            return Err(BackendError::Unsupported {
                reason: "x11 targeted capture lands later; use full".into(),
            }
            .tool(false));
        }
        let d = display().map_err(|e| e.tool(false))?;
        let raw = if has("maim") {
            run("screenshot", "maim", &["--format=png", "/dev/stdout"], &d)
                .await
                .map_err(|e| e.tool(true))?
        } else if has("import") {
            run("screenshot", "import", &["-window", "root", "png:-"], &d)
                .await
                .map_err(|e| e.tool(true))?
        } else {
            return Err(BackendError::Unavailable {
                backend: "x11-maim",
                detail: "neither maim nor import found".into(),
            }
            .tool(false));
        };
        let img = image::load_from_memory(&raw).map_err(|e| {
            BackendError::ExternalCommandFailed {
                stderr: format!("decode x11 screenshot: {e}"),
            }
            .tool(true)
        })?;
        let edge = max_long_edge.clamp(256, 1568);
        let (cw, ch) = (img.width(), img.height());
        let scale = (edge as f32 / cw.max(ch) as f32).min(1.0);
        let img = if scale < 1.0 {
            img.resize_exact(
                (cw as f32 * scale) as u32,
                (ch as f32 * scale) as u32,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| {
                BackendError::ExternalCommandFailed {
                    stderr: format!("png encode: {e}"),
                }
                .tool(true)
            })?;
        Ok(Shot {
            bytes: png,
            format: ShotFormat::Png,
            coord_w: cw,
            coord_h: ch,
        })
    }
}

// ---- Input ----

pub struct X11Input;

fn btn_arg(b: Button) -> &'static str {
    match b {
        Button::Left => "1",
        Button::Middle => "2",
        Button::Right => "3",
    }
}

#[async_trait::async_trait]
impl InputDriver for X11Input {
    fn id(&self) -> &'static str {
        "x11-xdotool"
    }
    async fn probe(&self) -> Probe {
        match display() {
            Err(e) => Probe {
                id: self.id(),
                ok: false,
                detail: e.tool(false).message,
            },
            Ok(_) if has("xdotool") => Probe {
                id: self.id(),
                ok: true,
                detail: "xdotool on PATH".into(),
            },
            Ok(_) => Probe {
                id: self.id(),
                ok: false,
                detail: "xdotool not found".into(),
            },
        }
    }
    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<keymap::Modifier>,
    ) -> Result<(), ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        // Modifiers via keydown/key around the click (xdotool natively supports this).
        for m in &hold {
            let k = match m {
                keymap::Modifier::Ctrl => "ctrl",
                keymap::Modifier::Shift => "shift",
                keymap::Modifier::Alt => "alt",
                keymap::Modifier::Super => "super",
            };
            run("mod down", "xdotool", &["keydown", k], &d)
                .await
                .map_err(|e| e.tool(true))?;
        }
        let r = run(
            "click",
            "xdotool",
            &[
                "mousemove",
                &x.to_string(),
                &y.to_string(),
                "click",
                btn_arg(button),
            ],
            &d,
        )
        .await;
        for m in hold.iter().rev() {
            let k = match m {
                keymap::Modifier::Ctrl => "ctrl",
                keymap::Modifier::Shift => "shift",
                keymap::Modifier::Alt => "alt",
                keymap::Modifier::Super => "super",
            };
            let _ = run("mod up", "xdotool", &["keyup", k], &d).await;
        }
        r.map(|_| ()).map_err(|e| e.tool(true))
    }
    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        run("move", "xdotool", &["mousemove", &x.to_string(), &y.to_string()], &d)
            .await
            .map(|_| ())
            .map_err(|e| e.tool(true))
    }
    async fn drag(&self, path: Vec<(i32, i32)>, button: Button) -> Result<(), ToolError> {
        let (start, _) = path.split_first().ok_or_else(|| {
            BackendError::Unsupported {
                reason: "drag needs ≥1 point".into(),
            }
            .tool(false)
        })?;
        let end = path.last().unwrap();
        let d = display().map_err(|e| e.tool(false))?;
        run(
            "drag",
            "xdotool",
            &[
                "mousemove",
                &start.0.to_string(),
                &start.1.to_string(),
                "mousedown",
                btn_arg(button),
                "mousemove",
                &end.0.to_string(),
                &end.1.to_string(),
                "mouseup",
                btn_arg(button),
            ],
            &d,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.tool(true))
    }
    async fn scroll(
        &self,
        x: i32,
        y: i32,
        _dx: i32,
        dy: i32,
        hold: Vec<keymap::Modifier>,
    ) -> Result<(), ToolError> {
        // xdotool buttons 4/5/6/7 = up/down/left/right; repeat per notch.
        let btn = if dy < 0 { "4" } else { "5" };
        let d = display().map_err(|e| e.tool(false))?;
        let key_name = |m: &keymap::Modifier| match m {
            keymap::Modifier::Ctrl => "ctrl",
            keymap::Modifier::Shift => "shift",
            keymap::Modifier::Alt => "alt",
            keymap::Modifier::Super => "super",
        };
        for m in &hold {
            run("mod down", "xdotool", &["keydown", key_name(m)], &d)
                .await
                .map_err(|e| e.tool(true))?;
        }
        let r = run(
            "scroll",
            "xdotool",
            &[
                "mousemove",
                &x.to_string(),
                &y.to_string(),
                "click",
                "--repeat",
                "3",
                btn,
            ],
            &d,
        )
        .await;
        for m in hold.iter().rev() {
            let _ = run("mod up", "xdotool", &["keyup", key_name(m)], &d).await;
        }
        r.map(|_| ()).map_err(|e| e.tool(true))
    }
    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        run("type", "xdotool", &["type", "--delay", "0", "--", &text], &d)
            .await
            .map(|_| ())
            .map_err(|e| e.tool(true))
    }
    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(BackendError::Unsupported {
                reason: "empty chord".into(),
            }
            .tool(false));
        }
        let d = display().map_err(|e| e.tool(false))?;
        // xdotool key syntax already matches our chord syntax.
        let mut args = vec!["key", "--"];
        args.extend(keys.iter().map(|s| s.as_str()));
        run("key", "xdotool", &args, &d)
            .await
            .map(|_| ())
            .map_err(|e| e.tool(true))
    }
}

// ---- Windows (wmctrl) ----

pub struct X11Windows;

#[async_trait::async_trait]
impl WindowDriver for X11Windows {
    fn id(&self) -> &'static str {
        "x11-wmctrl"
    }
    async fn probe(&self) -> Probe {
        match display() {
            Err(e) => Probe {
                id: self.id(),
                ok: false,
                detail: e.tool(false).message,
            },
            Ok(_) if has("wmctrl") => Probe {
                id: self.id(),
                ok: true,
                detail: "wmctrl on PATH".into(),
            },
            Ok(_) => Probe {
                id: self.id(),
                ok: false,
                detail: "wmctrl not found".into(),
            },
        }
    }
    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        // wmctrl -l -p -G: id desktop pid x y w h host title...
        let raw = run("list", "wmctrl", &["-l", "-p", "-G"], &d)
            .await
            .map_err(|e| e.tool(true))?;
        let text = String::from_utf8_lossy(&raw);
        let mut out = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (id, _desk, _pid, x, y, w, h) = match (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) {
                (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f), Some(g)) => (a, b, c, d, e, f, g),
                _ => continue,
            };
            let title: String = {
                // id desk pid x y w h host title...
                let mut it = line.split_whitespace();
                for _ in 0..8 {
                    it.next();
                }
                it.collect::<Vec<_>>().join(" ")
            };
            let num = |s: &str| s.parse::<i32>().unwrap_or(0);
            out.push(WindowInfo {
                window_ref: Ref(format!("x11:{id}")),
                title,
                class: String::new(),
                geometry: Bbox {
                    x: num(x),
                    y: num(y),
                    w: num(w).max(0) as u32,
                    h: num(h).max(0) as u32,
                },
                screen: 0,
                is_active: false,
            });
            let _ = (_desk, _pid);
        }
        Ok(out)
    }
    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        let wid = id.0.strip_prefix("x11:").unwrap_or(&id.0);
        let d = display().map_err(|e| e.tool(false))?;
        run("focus", "xdotool", &["windowactivate", "--sync", wid], &d)
            .await
            .map(|_| ())
            .map_err(|e| e.tool(true))
    }
    async fn minimize(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "wmctrl minimize needs numeric id plumbing; use focus/close".into(),
        }
        .tool(false))
    }
    async fn maximize(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "maximize via wmctrl lands later".into(),
        }
        .tool(false))
    }
    async fn restore(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "restore via wmctrl lands later".into(),
        }
        .tool(false))
    }
    async fn close(&self, id: &Ref) -> Result<(), ToolError> {
        let wid = id.0.strip_prefix("x11:").unwrap_or(&id.0);
        let d = display().map_err(|e| e.tool(false))?;
        run("close", "xdotool", &["windowclose", "--sync", wid], &d)
            .await
            .map(|_| ())
            .map_err(|e| e.tool(true))
    }
    async fn move_resize(&self, _id: &Ref, _geo: Bbox) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "move/resize via wmctrl -r -e lands later".into(),
        }
        .tool(false))
    }
}
