use super::*;
use serde_json::Value;

/// Blender takes the foreground; returns its active window so a later
/// check can prove the background edit left it alone. A key press does that
/// on a Wayland seat, where input goes through the focused window. On an X11
/// seat input reaches the window without activating it, so the window
/// manager is asked to activate Blender the way a taskbar would.
async fn blender_active(seat: &Seat, client: &mut Joiner) -> Result<Value> {
    let blender = agent_desktop::desktop::apps::find("blender.desktop")
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let owned = |window: &Value| {
        serde_json::from_value::<agent_desktop::platform::drivers::WindowInfo>(window.clone())
            .is_ok_and(|window| blender.owns(&window))
    };
    client
        .eval("const blender = await agentdesktop.getApp('blender.desktop');")
        .await?;
    if seat.environment.get("XDG_SESSION_TYPE").map(String::as_str) == Some("x11") {
        let state = client.json("await agentdesktop.getState();").await?;
        let window = state["windows"]
            .as_array()
            .context("windows")?
            .iter()
            .find(|window| owned(window))
            .context("Blender window")?;
        let id = window["window_ref"]
            .as_str()
            .and_then(|window_ref| window_ref.strip_prefix("x11:"))
            .context("Blender X11 window")?;
        let status = tokio::process::Command::new("wmctrl")
            .args(["-i", "-a", id])
            .envs(&seat.environment)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "wmctrl activates Blender");
    } else {
        client.eval("await blender.pressKey('Escape');").await?;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = client.json("await agentdesktop.getState();").await?;
        let active = state["windows"]
            .as_array()
            .context("windows")?
            .iter()
            .find(|window| window["is_active"] == true);
        if let Some(active) = active.filter(|window| owned(window)) {
            return Ok(active.clone());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Blender has focus before the background edit"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn still_active(client: &mut Joiner, active: &Value) -> Result<()> {
    let state = client.json("await agentdesktop.getState();").await?;
    anyhow::ensure!(
        state["windows"]
            .as_array()
            .context("windows")?
            .iter()
            .any(|window| window["is_active"] == true
                && window["window_ref"] == active["window_ref"]),
        "background editing retained Blender's focus"
    );
    Ok(())
}

fn shows_value(state: &Value, value: &str) -> Result<bool> {
    Ok(state["elements"]
        .as_array()
        .context("elements")?
        .iter()
        .any(|element| element["value"]["text"] == value))
}

fn dimension_control(state: &Value) -> Result<&Value> {
    state["elements"]
        .as_array()
        .context("elements")?
        .iter()
        .find(|element| element["role"]["text"] == "spin button")
        .context("dimension control")
}

#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Krita and Blender"]
async fn qa_background_semantic_edit() -> Result<()> {
    let seat = Seat::boot("background-edit").await?;
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
        &[],
    )
    .await?;
    client
        .eval("const krita = await agentdesktop.getApp('org.kde.krita.desktop');")
        .await?;
    new_krita_document(&mut client, "krita").await?;
    let state = client.json("await krita.getAXState();").await?;
    let index = dimension_control(&state)?["index"]
        .as_u64()
        .context("index")?;
    let active = blender_active(&seat, &mut client).await?;
    client
        .eval(&format!("await krita.setValue({index},'512');"))
        .await?;
    let edited = client.json("await krita.getAXState();").await?;
    anyhow::ensure!(shows_value(&edited, "512")?, "background value changed");
    still_active(&mut client, &active).await?;
    std::fs::write(
        seat.artifacts.join("background-edit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"active":active,"edited":edited}))?,
    )?;
    client.close().await?;
    Ok(())
}

async fn capabilities(client: &mut Joiner) -> Result<Value> {
    Ok(client.json("await agentdesktop.getState();").await?["capabilities"].clone())
}

/// Clicks Krita's width field at coordinates derived from its accessibility
/// bounds and the dialog screenshot, selects the text and types a new size
/// while Blender stays the active window; the pointer never has to visit
/// the dialog. The dialog must be a `protocol` client.
async fn edit_by_coordinates(
    seat: &Seat,
    client: &mut Joiner,
    capabilities: &Value,
    protocol: &str,
    record: &str,
) -> Result<()> {
    client
        .eval("const krita = await agentdesktop.getApp('org.kde.krita.desktop');")
        .await?;
    new_krita_document(client, "krita").await?;
    let state = client.json("await krita.getAXState();").await?;
    anyhow::ensure!(
        state["window"]["client_protocol"] == protocol,
        "Krita's dialog is an {protocol} window: {}",
        state["window"]
    );
    let bbox: [f64; 4] = serde_json::from_value(dimension_control(&state)?["bbox"].clone())
        .context("dimension control bounds")?;
    let geometry: agent_desktop::platform::drivers::Bbox =
        serde_json::from_value(state["window"]["geometry"].clone()).context("window geometry")?;
    // The screenshot comes first: GNOME activates the window to capture it.
    let (width, height) = client
        .screenshot("krita", &seat.artifacts.join("dialog.png"))
        .await?;
    let active = blender_active(seat, client).await?;
    let x = (bbox[0] + bbox[2] / 2.0 - f64::from(geometry.x)) * f64::from(width)
        / f64::from(geometry.w);
    let y = (bbox[1] + bbox[3] / 2.0 - f64::from(geometry.y)) * f64::from(height)
        / f64::from(geometry.h);
    client
        .eval(&format!(
            "await krita.click([{},{}]); await krita.pressKey('ctrl+a'); await krita.typeText('512'); await krita.pressKey('Tab');",
            x.round(),
            y.round()
        ))
        .await?;
    let edited = client.json("await krita.getAXState();").await?;
    anyhow::ensure!(
        shows_value(&edited, "512")?,
        "background click and typing changed the value"
    );
    still_active(client, &active).await?;
    std::fs::write(
        seat.artifacts.join(format!("{record}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "capabilities": capabilities,
            "window": state["window"],
            "active": active,
            "edited": edited,
        }))?,
    )?;
    Ok(())
}

/// X11 seats deliver coordinate input to every window.
#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Krita and Blender"]
async fn qa_background_coordinate_input() -> Result<()> {
    let seat = Seat::boot("background-coordinate").await?;
    if seat.environment.get("XDG_SESSION_TYPE").map(String::as_str) != Some("x11") {
        eprintln!("skipping: window-targeted input needs an X11 seat");
        return Ok(());
    }
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
        &[],
    )
    .await?;
    let capabilities = capabilities(&mut client).await?;
    anyhow::ensure!(
        capabilities["coordinateInput"] == "window",
        "X11 seat addresses windows: {capabilities}"
    );
    edit_by_coordinates(
        &seat,
        &mut client,
        &capabilities,
        "x11",
        "background-coordinate",
    )
    .await?;
    client.close().await?;
    Ok(())
}

/// Wayland seats reach Xwayland clients the same way: Krita started on the
/// xcb platform plugin takes the edit while native windows keep the
/// foreground path.
#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Krita and Blender"]
async fn qa_background_xwayland_input() -> Result<()> {
    let seat = Seat::boot("background-xwayland").await?;
    if seat.environment.get("XDG_SESSION_TYPE").map(String::as_str) != Some("wayland")
        || !seat.environment.contains_key("DISPLAY")
    {
        eprintln!("skipping: Xwayland targeting needs a Wayland seat with DISPLAY");
        return Ok(());
    }
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
        &[("QT_QPA_PLATFORM", "xcb")],
    )
    .await?;
    let capabilities = capabilities(&mut client).await?;
    anyhow::ensure!(
        capabilities["coordinateInput"] == "foreground"
            && capabilities["targetedClients"] == "xwayland",
        "Wayland seat targets Xwayland clients only: {capabilities}"
    );
    edit_by_coordinates(
        &seat,
        &mut client,
        &capabilities,
        "x11",
        "background-xwayland",
    )
    .await?;
    client.close().await?;
    Ok(())
}
