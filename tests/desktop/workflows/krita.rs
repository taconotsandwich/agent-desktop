use super::*;

#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Krita"]
async fn qa_krita_workflow() -> Result<()> {
    let seat = Seat::boot("krita-workflow").await?;
    eprintln!("QA artifacts: {}", seat.artifacts.display());
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
    )
    .await?;
    client
        .eval("const app = await agentdesktop.getApp('org.kde.krita.desktop');")
        .await?;
    new_krita_document(&mut client, "app").await?;
    for (position, new) in [(0, "640"), (1, "480")] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let index = loop {
            let state = client.json("await app.getAXState();").await?;
            let mut controls: Vec<_> = state["elements"]
                .as_array()
                .context("elements")?
                .iter()
                .filter(|element| {
                    element["role"]["text"] == "spin button"
                        && element["bbox"][2].as_i64().is_some_and(|width| width > 0)
                })
                .collect();
            controls
                .sort_by_key(|element| (element["bbox"][1].as_i64(), element["bbox"][0].as_i64()));
            if let Some(element) = controls.get(position) {
                break element["index"].as_u64().context("spin index")?;
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "dimension controls did not appear"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        client
            .eval(&format!("await app.setValue({index},{new:?});"))
            .await?;
    }
    let state = client.json("await app.getAXState();").await?;
    let create = element(&state, "button", "Create")?;
    client.eval(&format!("await app.click({create});")).await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    save_state(&mut client, "app", &seat.artifacts.join("created.json")).await?;
    client
        .screenshot("app", &seat.artifacts.join("created.png"))
        .await?;
    client.eval("await app.drag([300,300],[650,500]);").await?;
    client
        .screenshot("app", &seat.artifacts.join("painted.png"))
        .await?;
    client.eval("await app.pressKey('ctrl+z');").await?;
    client
        .screenshot("app", &seat.artifacts.join("undone.png"))
        .await?;
    client.eval("await app.pressKey('ctrl+shift+z');").await?;
    client.eval("await app.pressKey('ctrl+shift+s');").await?;
    let state = save_state(&mut client, "app", &seat.artifacts.join("save-dialog.json")).await?;
    client
        .screenshot("app", &seat.artifacts.join("save-dialog.png"))
        .await?;
    let input = filename_input(&state)?;
    let document = seat.artifacts.join("桌面-QA.kra");
    client
        .eval(&format!(
            "await app.setValue({input},{});",
            serde_json::to_string(&document)?
        ))
        .await?;
    let state = client.json("await app.getAXState();").await?;
    let save = element(&state, "button", "Save")?;
    client.eval(&format!("await app.click({save});")).await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let merged = loop {
        let result = tokio::process::Command::new("unzip")
            .arg("-p")
            .arg(&document)
            .arg("mergedimage.png")
            .kill_on_drop(true)
            .output()
            .await?;
        if result.status.success() {
            break image::load_from_memory(&result.stdout)?.to_rgb8();
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Krita did not save a valid document"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    anyhow::ensure!(
        merged.dimensions() == (640, 480),
        "saved document dimensions"
    );
    anyhow::ensure!(
        merged
            .pixels()
            .filter(|pixel| pixel.0.iter().all(|value| *value < 100))
            .count()
            > 100,
        "saved paint stroke"
    );
    merged.save(seat.artifacts.join("merged.png"))?;
    client
        .eval("await app.pressKey('ctrl+w'); await app.pressKey('ctrl+o');")
        .await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = client.json("await app.getAXState();").await?;
        if state["window"]["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("Open Images"))
        {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "open dialog did not appear"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let document_json = serde_json::to_string(&document)?;
    let state = {
        let mut pasted = None;
        for _ in 0..3 {
            let state = client.json("await app.getAXState();").await?;
            let input = filename_input(&state)?;
            client
                .eval(&format!(
                    "await app.click({input}); await app.pressKey('ctrl+a'); await app.paste({document_json});"
                ))
                .await?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let state = client.json("await app.getAXState();").await?;
                if state["elements"]
                    .as_array()
                    .context("elements")?
                    .iter()
                    .any(|row| row["value"]["text"].as_str() == document.to_str())
                {
                    pasted = Some(state);
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            if pasted.is_some() {
                break;
            }
        }
        pasted.context("Unicode paste appears in the open dialog")?
    };
    let open = element(&state, "button", "Open")?;
    client.eval(&format!("await app.click({open});")).await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = save_state(&mut client, "app", &seat.artifacts.join("reopened.json")).await?;
        if state["elements"]
            .as_array()
            .context("elements")?
            .iter()
            .any(|row| {
                row["name"]["text"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("桌面-QA.kra"))
            })
        {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Krita did not reopen the saved document"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    client
        .screenshot("app", &seat.artifacts.join("reopened.png"))
        .await?;
    client.close().await?;
    Ok(())
}
