use super::*;
use serde_json::Value;

/// Blender takes the foreground with a real key press; returns the active
/// window so a later check can prove the background edit left it alone.
async fn blender_active(client: &mut Joiner) -> Result<Value> {
    client
        .eval("const blender = await agentdesktop.getApp('blender.desktop'); await blender.pressKey('Escape');")
        .await?;
    let state = client.json("await agentdesktop.getState();").await?;
    let active = state["windows"]
        .as_array()
        .context("windows")?
        .iter()
        .find(|window| window["is_active"] == true)
        .context("active app")?
        .clone();
    anyhow::ensure!(
        agent_desktop::desktop::apps::find("blender.desktop")
            .map_err(|error| anyhow::anyhow!(error.message))?
            .owns(&serde_json::from_value(active.clone())?),
        "Blender has focus before the background edit"
    );
    Ok(active)
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
    let active = blender_active(&mut client).await?;
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

/// X11 seats deliver coordinate input to the window itself: click Krita's
/// width field, select its text and type a new size while Blender stays the
/// active window and the pointer never has to visit the dialog.
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
    )
    .await?;
    let capabilities = client.json("await agentdesktop.getState();").await?["capabilities"].clone();
    anyhow::ensure!(
        capabilities["coordinateInput"] == "window",
        "X11 seat addresses windows: {capabilities}"
    );
    client
        .eval("const krita = await agentdesktop.getApp('org.kde.krita.desktop');")
        .await?;
    new_krita_document(&mut client, "krita").await?;
    let active = blender_active(&mut client).await?;
    let state = client.json("await krita.getAXState();").await?;
    let bbox: [f64; 4] = serde_json::from_value(dimension_control(&state)?["bbox"].clone())
        .context("dimension control bounds")?;
    let geometry: agent_desktop::platform::drivers::Bbox =
        serde_json::from_value(state["window"]["geometry"].clone()).context("window geometry")?;
    let (width, height) = client
        .screenshot("krita", &seat.artifacts.join("dialog.png"))
        .await?;
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
    still_active(&mut client, &active).await?;
    std::fs::write(
        seat.artifacts.join("background-coordinate.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"capabilities":capabilities,"active":active,"edited":edited}),
        )?,
    )?;
    client.close().await?;
    Ok(())
}
