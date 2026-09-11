mod support;
mod workflows;
use anyhow::{Context, Result};
use support::{Seat, mcp::Joiner, server_bin};

#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Krita and Blender"]
async fn qa_application_discovery() -> Result<()> {
    let seat = Seat::boot("discovery").await?;
    eprintln!("QA artifacts: {}", seat.artifacts.display());
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
    )
    .await?;
    let state = client.json("await agentdesktop.getState();").await?;
    std::fs::write(
        seat.artifacts.join("desktop.json"),
        serde_json::to_vec_pretty(&state)?,
    )?;
    for (name, binding) in [
        ("org.kde.krita.desktop", "krita"),
        ("blender.desktop", "blender"),
    ] {
        let state = client
            .json(&format!("const {binding} = await agentdesktop.getApp({name:?});"))
            .await?;
        std::fs::write(
            seat.artifacts.join(format!("{binding}.json")),
            serde_json::to_vec_pretty(&state)?,
        )?;
        anyhow::ensure!(
            state["window"]["pid"].as_u64().is_some(),
            "{name} has a process identity"
        );
        let (w, h) = client
            .screenshot(binding, &seat.artifacts.join(format!("{binding}.png")))
            .await?;
        anyhow::ensure!(w > 100 && h > 100, "{name} screenshot dimensions");
        let fresh = client
            .json(&format!("await {binding}.getAXState();"))
            .await?;
        std::fs::write(
            seat.artifacts.join(format!("{binding}-fresh.json")),
            serde_json::to_vec_pretty(&fresh)?,
        )?;
        anyhow::ensure!(
            fresh["window"]["pid"] == state["window"]["pid"],
            "fresh observation belongs to the same application process"
        );
        let elements = fresh["elements"].as_array().context("elements")?;
        eprintln!(
            "{name}: {} accessibility elements, warnings: {}",
            elements.len(),
            fresh["warnings"]
        );
        if binding == "krita" {
            client.eval("await krita.pressKey('ctrl+n');").await?;
            let dialog = client.json("await krita.getAXState();").await?;
            std::fs::write(
                seat.artifacts.join("krita-new-document.json"),
                serde_json::to_vec_pretty(&dialog)?,
            )?;
            client
                .screenshot("krita", &seat.artifacts.join("krita-new-document.png"))
                .await?;
            client.eval("await krita.pressKey('Escape');").await?;
        }
    }
    client.close().await?;
    Ok(())
}
