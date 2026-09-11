use super::*;

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
    let index = state["elements"]
        .as_array()
        .context("elements")?
        .iter()
        .find(|element| element["role"]["text"] == "spin button")
        .context("dimension control")?["index"]
        .as_u64()
        .context("index")?;
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
        "Blender has focus before semantic edit"
    );
    client
        .eval(&format!("await krita.setValue({index},'512');"))
        .await?;
    let edited = client.json("await krita.getAXState();").await?;
    anyhow::ensure!(
        edited["elements"]
            .as_array()
            .context("elements")?
            .iter()
            .any(|element| element["value"]["text"] == "512"),
        "background value changed"
    );
    let state = client.json("await agentdesktop.getState();").await?;
    anyhow::ensure!(
        state["windows"]
            .as_array()
            .context("windows")?
            .iter()
            .any(|window| window["is_active"] == true
                && window["window_ref"] == active["window_ref"]),
        "semantic editing retained Blender's focus"
    );
    std::fs::write(
        seat.artifacts.join("background-edit.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"active":active,"edited":edited}))?,
    )?;
    client.close().await?;
    Ok(())
}
