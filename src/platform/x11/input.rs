use super::{display, has, run};
use crate::{
    error::BackendError,
    platform::{
        drivers::{Button, InputDriver, Probe},
        keymap,
    },
    types::ToolError,
};

pub struct X11Input;

struct ReleaseInput {
    display: String,
    keys: Vec<String>,
    buttons: Vec<String>,
}
impl ReleaseInput {
    fn new(display: &str) -> Self {
        Self {
            display: display.into(),
            keys: vec![],
            buttons: vec![],
        }
    }
}
impl Drop for ReleaseInput {
    fn drop(&mut self) {
        let mut args = Vec::new();
        for key in &self.keys {
            args.extend(["keyup", key.as_str()]);
        }
        for button in &self.buttons {
            args.extend(["mouseup", button.as_str()]);
        }
        if args.is_empty() {
            return;
        }
        let Ok(mut child) = std::process::Command::new("xdotool")
            .args(args)
            .env("DISPLAY", &self.display)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while matches!(child.try_wait(), Ok(None)) {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

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
            Ok(_) if has("xdotool") && has("xmodmap") => Probe {
                id: self.id(),
                ok: true,
                detail: "xdotool and xmodmap on PATH".into(),
            },
            Ok(_) => Probe {
                id: self.id(),
                ok: false,
                detail: "xdotool and xmodmap are required".into(),
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
        let mut release = ReleaseInput::new(&d);
        release.buttons.push(btn_arg(button).into());
        for m in &hold {
            let k = match m {
                keymap::Modifier::Ctrl => "ctrl",
                keymap::Modifier::Shift => "shift",
                keymap::Modifier::Alt => "alt",
                keymap::Modifier::Super => "super",
            };
            release.keys.push(k.into());
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
                "sleep",
                "0.02",
                "mousedown",
                btn_arg(button),
                "sleep",
                "0.03",
                "mouseup",
                btn_arg(button),
            ],
            &d,
        )
        .await;
        if r.is_ok() {
            release.buttons.clear();
        }
        for m in hold.iter().rev() {
            let k = match m {
                keymap::Modifier::Ctrl => "ctrl",
                keymap::Modifier::Shift => "shift",
                keymap::Modifier::Alt => "alt",
                keymap::Modifier::Super => "super",
            };
            run("mod up", "xdotool", &["keyup", k], &d)
                .await
                .map_err(|error| error.tool(true))?;
            release.keys.retain(|key| key != k);
        }
        r.map(|_| ()).map_err(|e| e.tool(true))
    }
    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        run(
            "move",
            "xdotool",
            &["mousemove", &x.to_string(), &y.to_string()],
            &d,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.tool(true))
    }
    async fn drag(
        &self,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError> {
        let (start, rest) = path.split_first().ok_or_else(|| {
            BackendError::Unsupported {
                reason: "drag needs ≥1 point".into(),
            }
            .tool(false)
        })?;
        let d = display().map_err(|e| e.tool(false))?;
        // One xdotool invocation: move, press, dwell (xdotool sleep takes
        // seconds), interpolated moves, release.
        let mut release = ReleaseInput::new(&d);
        release.buttons.push(btn_arg(button).into());
        let mut args: Vec<String> = vec![
            "mousemove".into(),
            start.0.to_string(),
            start.1.to_string(),
            "mousedown".into(),
            btn_arg(button).into(),
            "sleep".into(),
            format!("{:.2}", dwell_ms as f64 / 1000.0),
        ];
        let mut prev = *start;
        for &(ex, ey) in rest {
            for i in 1..=10 {
                let fx = prev.0 + (ex - prev.0) * i / 10;
                let fy = prev.1 + (ey - prev.1) * i / 10;
                args.push("mousemove".into());
                args.push(fx.to_string());
                args.push(fy.to_string());
                args.push("sleep".into());
                args.push(format!("{:.2}", step_ms as f64 / 1000.0));
            }
            prev = (ex, ey);
        }
        args.push("mouseup".into());
        args.push(btn_arg(button).into());
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run("drag", "xdotool", &arg_refs, &d)
            .await
            .map_err(|e| e.tool(true))?;
        release.buttons.clear();
        Ok(())
    }
    async fn scroll(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        hold: Vec<keymap::Modifier>,
    ) -> Result<(), ToolError> {
        // xdotool buttons 4/5/6/7 = up/down/left/right; repeat per notch.
        let d = display().map_err(|e| e.tool(false))?;
        let key_name = |m: &keymap::Modifier| match m {
            keymap::Modifier::Ctrl => "ctrl",
            keymap::Modifier::Shift => "shift",
            keymap::Modifier::Alt => "alt",
            keymap::Modifier::Super => "super",
        };
        let mut release = ReleaseInput::new(&d);
        for m in &hold {
            release.keys.push(key_name(m).into());
            run("mod down", "xdotool", &["keydown", key_name(m)], &d)
                .await
                .map_err(|e| e.tool(true))?;
        }
        let r: Result<(), BackendError> = async {
            run(
                "move",
                "xdotool",
                &["mousemove", &x.to_string(), &y.to_string()],
                &d,
            )
            .await?;
            for (delta, negative, positive) in [(dx, "6", "7"), (dy, "4", "5")] {
                if delta == 0 {
                    continue;
                }
                let count = delta.unsigned_abs().div_ceil(120).max(1).to_string();
                run(
                    "scroll",
                    "xdotool",
                    &[
                        "click",
                        "--repeat",
                        &count,
                        if delta < 0 { negative } else { positive },
                    ],
                    &d,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        for m in hold.iter().rev() {
            let _ = run("mod up", "xdotool", &["keyup", key_name(m)], &d).await;
        }
        r.map(|_| ()).map_err(|e| e.tool(true))
    }
    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        let d = display().map_err(|e| e.tool(false))?;
        run(
            "type",
            "xdotool",
            &["type", "--delay", "10", "--", &text],
            &d,
        )
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
        let mut keys: Vec<String> = keys
            .iter()
            .map(|key| x11_chord(key))
            .collect::<Result<_, _>>()
            .map_err(|error| error.tool(false))?;
        for key in &mut keys {
            let name = key.rsplit('+').next().unwrap();
            if name.starts_with('F') && name[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))
            {
                // libxdo can select an Alt mapping for an unmodified function key.
                // Resolve its level-one keycode without synthesizing extra modifiers.
                let mapping = run("keyboard mapping", "xmodmap", &["-pke"], &d)
                    .await
                    .map_err(|error| error.tool(false))?;
                let code = unmodified_keycode(&String::from_utf8_lossy(&mapping), name)
                    .ok_or_else(|| {
                        crate::error::fail(
                            "unsupported",
                            format!("No unmodified X11 mapping for {name}"),
                        )
                    })?;
                let prefix = key.len() - name.len();
                key.replace_range(prefix.., &code.to_string());
            }
        }
        let mut release = ReleaseInput::new(&d);
        release.keys = keys
            .iter()
            .flat_map(|chord| chord.split('+').map(str::to_owned))
            .collect();
        let mut args = vec!["key", "--delay", "40", "--"];
        args.extend(keys.iter().map(|s| s.as_str()));
        run("key", "xdotool", &args, &d)
            .await
            .map_err(|e| e.tool(true))?;
        release.keys.clear();
        Ok(())
    }
}

fn unmodified_keycode(mapping: &str, name: &str) -> Option<u8> {
    mapping.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        if parts.next()? != "keycode" {
            return None;
        }
        let code = parts.next()?.parse().ok()?;
        (parts.next()? == "=" && parts.next()? == name).then_some(code)
    })
}

fn x11_chord(value: &str) -> Result<String, BackendError> {
    let chord = keymap::parse_chord(value)?;
    let mut names: Vec<String> = chord
        .modifiers
        .iter()
        .map(|modifier| {
            match modifier {
                keymap::Modifier::Ctrl => "ctrl",
                keymap::Modifier::Alt => "alt",
                keymap::Modifier::Shift => "shift",
                keymap::Modifier::Super => "super",
            }
            .into()
        })
        .collect();
    let raw = value
        .rsplit('+')
        .next()
        .unwrap()
        .trim()
        .to_ascii_lowercase();
    let key = match chord.key {
        keymap::KEY_ENTER => "Return",
        keymap::KEY_ESC => "Escape",
        keymap::KEY_BACKSPACE => "BackSpace",
        keymap::KEY_TAB => "Tab",
        keymap::KEY_SPACE => "space",
        keymap::KEY_DELETE => "Delete",
        keymap::KEY_INSERT => "Insert",
        keymap::KEY_HOME => "Home",
        keymap::KEY_END => "End",
        keymap::KEY_UP => "Up",
        keymap::KEY_DOWN => "Down",
        keymap::KEY_LEFT => "Left",
        keymap::KEY_RIGHT => "Right",
        keymap::KEY_PAGEUP => "Prior",
        keymap::KEY_PAGEDOWN => "Next",
        12 => "minus",
        13 => "equal",
        26 => "bracketleft",
        27 => "bracketright",
        39 => "semicolon",
        40 => "apostrophe",
        41 => "grave",
        43 => "backslash",
        51 => "comma",
        52 => "period",
        53 => "slash",
        keymap::KEY_F1..=keymap::KEY_F10 | keymap::KEY_F11 | keymap::KEY_F12 => {
            names.push(raw.to_ascii_uppercase());
            return Ok(names.join("+"));
        }
        _ => &raw,
    };
    names.push(key.into());
    Ok(names.join("+"))
}
