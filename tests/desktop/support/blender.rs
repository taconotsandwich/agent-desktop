use super::mcp::{Joiner, changed_pixels, save_state};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, path::Path};

pub async fn workflow(
    client: &mut Joiner,
    artifacts: &Path,
    environment: &BTreeMap<String, String>,
    label: &str,
    offset: i32,
) -> Result<()> {
    client
        .eval("const app = await agentdesktop.getApp('blender.desktop');")
        .await?;
    let path = artifacts.join("ready.png");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        client.screenshot("app", &path).await?;
        let pixels = image::open(&path)?.to_rgb8();
        let colorful = pixels
            .pixels()
            .filter(|pixel| pixel.0.iter().max().unwrap() - pixel.0.iter().min().unwrap() > 40)
            .count();
        if colorful > 3000 {
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Blender did not render its interface"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    save_state(client, "app", &artifacts.join("ready.json")).await?;
    client.eval("await app.pressKey('F3');").await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let setup = artifacts.join("setup-f3.png");
    client.screenshot("app", &setup).await?;
    eprintln!(
        "{label}: F3 during Quick Setup changed {} pixels",
        changed_pixels(&path, &setup)?
    );
    let pixels = image::open(&path)?.to_rgb8();
    let x = pixels.width() / 2;
    let mut run_start = None;
    let mut candidates = Vec::new();
    for y in pixels.height() / 2..pixels.height() * 4 / 5 {
        let button = (x - 75..x - 55).all(|column| {
            let color = pixels.get_pixel(column, y).0;
            (70..=120).contains(&color[0])
                && color.iter().max().unwrap() - color.iter().min().unwrap() <= 5
        });
        if button {
            run_start.get_or_insert(y);
        } else if let Some(start) = run_start.take()
            && (6..=30).contains(&(y - start))
        {
            candidates.push((y - start, (start + y) / 2));
        }
    }
    let y = candidates
        .into_iter()
        .max_by_key(|candidate| candidate.0)
        .context("visible Quick Setup Continue button")?
        .1;
    client.eval(&format!("await app.click([{x},{y}]);")).await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    client.eval("await app.pressKey('Escape');").await?;
    let baseline = artifacts.join("search-before.png");
    client.screenshot("app", &baseline).await?;
    client.eval("await app.pressKey('F3');").await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let path = artifacts.join("search-open.png");
        client.screenshot("app", &path).await?;
        let changed = changed_pixels(&baseline, &path)?;
        if changed > 10_000 {
            eprintln!("{label}: F3 changed {changed} pixels after Quick Setup");
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "F3 did not visibly open Blender search"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    for key in ["Escape", "g", "x", &offset.to_string(), "Enter", "F2"] {
        client
            .eval(&format!("await app.pressKey({key:?});"))
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        client
            .screenshot("app", &artifacts.join(format!("key-{key}.png")))
            .await?;
    }
    client
        .eval(&format!(
            "await app.typeText({}); await app.pressKey('Enter');",
            serde_json::to_string(label)?
        ))
        .await?;
    client
        .screenshot("app", &artifacts.join("edited.png"))
        .await?;
    save_scene(client, artifacts).await?;
    let document = artifacts.join("desktop-qa.blend");
    let folder = serde_json::to_string(artifacts)?;
    let verify = artifacts.join("verify.py");
    std::fs::write(
        &verify,
        format!(
            "import bpy\nobj = bpy.data.objects.get({})\nassert obj is not None, 'renamed cube missing'\nassert abs(obj.location.x - {offset}) < 0.0001, 'cube position was not saved'\nassert obj.type == 'MESH'\nprint('Saved scene verified')\n",
            serde_json::to_string(label)?
        ),
    )?;
    let verified = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new("blender")
            .args(["--background", "--factory-startup"])
            .arg(&document)
            .args(["--python-exit-code", "1", "--python"])
            .arg(&verify)
            .envs(environment)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    std::fs::write(
        artifacts.join("verification.log"),
        [verified.stdout, verified.stderr].concat(),
    )?;
    anyhow::ensure!(
        verified.status.success(),
        "saved Blender scene verification failed"
    );
    client.eval("await app.pressKey('ctrl+o');").await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (dialog_width, dialog_height) = client
        .screenshot("app", &artifacts.join("open-dialog.png"))
        .await?;
    client.eval(&format!("await app.click([{},{}]); await app.pressKey('ctrl+l'); await app.pressKey('ctrl+a'); await app.typeText({folder}); await app.pressKey('Enter');",dialog_width/2,dialog_height/2)).await?;
    client.eval(&format!("await app.click([{},{}]); await app.pressKey('ctrl+a'); await app.typeText('desktop-qa.blend'); await app.pressKey('Enter'); await app.click([{},{}]);",dialog_width/2,dialog_height-22,dialog_width-70,dialog_height-22)).await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    client
        .screenshot("app", &artifacts.join("reopened.png"))
        .await?;
    let state = save_state(client, "app", &artifacts.join("reopened.json")).await?;
    anyhow::ensure!(
        state["window"]["title"]
            .as_str()
            .is_some_and(|title| title.contains("desktop-qa.blend")),
        "Blender reopened the scene"
    );
    Ok(())
}

/// Drive Blender's file view until the scene lands in the artifacts directory.
/// The dialog is a separate XWayland window with no accessibility tree, and
/// under software compositing the first input can arrive before Blender has
/// taken focus, leaving the dialog at its defaults; redo the sequence then.
async fn save_scene(client: &mut Joiner, artifacts: &Path) -> Result<()> {
    let document = artifacts.join("desktop-qa.blend");
    let folder = serde_json::to_string(artifacts)?;
    for attempt in 1..=3 {
        client.eval("await app.pressKey('ctrl+shift+s');").await?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let (dialog_width, dialog_height) = client
            .screenshot("app", &artifacts.join("save-dialog.png"))
            .await?;
        if attempt == 1 {
            save_state(client, "app", &artifacts.join("save-dialog.json")).await?;
            let desktop = client.json("await agentdesktop.getState();").await?;
            std::fs::write(
                artifacts.join("desktop.json"),
                serde_json::to_vec_pretty(&desktop)?,
            )?;
        }
        let state = client.json("await app.getAXState();").await?;
        anyhow::ensure!(
            state["window"]["title"] == "Blender File View",
            "save dialog appeared"
        );
        client.eval(&format!("await app.click([{},{}]); await app.pressKey('ctrl+l'); await app.pressKey('ctrl+a'); await app.typeText({folder}); await app.pressKey('Enter');",dialog_width/2,dialog_height/2)).await?;
        client.eval(&format!("await app.click([{},{}]); await app.pressKey('ctrl+a'); await app.typeText('desktop-qa.blend'); await app.pressKey('Enter');",dialog_width/2,dialog_height-22)).await?;
        client
            .screenshot("app", &artifacts.join("save-ready.png"))
            .await?;
        client
            .eval(&format!(
                "await app.click([{},{}]);",
                dialog_width - 70,
                dialog_height - 22
            ))
            .await?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::fs::metadata(&document).is_ok_and(|metadata| metadata.len() > 10_000) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        client
            .screenshot("app", &artifacts.join(format!("save-result-{attempt}.png")))
            .await?;
    }
    anyhow::bail!("Blender did not save its scene")
}
