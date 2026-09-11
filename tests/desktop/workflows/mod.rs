use crate::support::mcp::save_state;
use crate::support::{Seat, mcp::Joiner, server_bin};
use anyhow::{Context, Result};
mod background;
mod blender;
mod krita;

fn element(state: &serde_json::Value, role: &str, name: &str) -> Result<u64> {
    state["elements"]
        .as_array()
        .context("elements")?
        .iter()
        .find(|element| element["role"]["text"] == role && element["name"]["text"] == name)
        .with_context(|| format!("Missing {role} {name}"))?["index"]
        .as_u64()
        .context("element index")
}

fn filename_input(state: &serde_json::Value) -> Result<u64> {
    let elements = state["elements"].as_array().context("elements")?;
    let label = elements
        .iter()
        .find(|row| matches!(row["name"]["text"].as_str(), Some("Name:" | "File name:")))
        .context("filename label")?;
    let input = elements
        .iter()
        .find(|row| {
            if row["role"]["text"] != "text" {
                return false;
            }
            if row["parent_id"] == label["id"] {
                return true;
            }
            let center = label["bbox"][1]
                .as_i64()
                .zip(label["bbox"][3].as_i64())
                .map(|(y, h)| y + h / 2);
            center.is_some_and(|center| {
                row["bbox"][1]
                    .as_i64()
                    .zip(row["bbox"][3].as_i64())
                    .is_some_and(|(y, h)| center >= y && center < y + h)
            })
        })
        .context("filename text")?;
    input["index"].as_u64().context("filename index")
}

async fn new_krita_document(client: &mut Joiner, app: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let state = client.json(&format!("await {app}.getAXState();")).await?;
        if let Ok(index) = element(&state, "button", "New") {
            client.eval(&format!("try {{ await {app}.click({index}); }} catch(error) {{ if(error.code !== 'action_pending') throw error; }}")).await?;
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Krita's New button did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    loop {
        let state = client.json(&format!("await {app}.getAXState();")).await?;
        if state["elements"]
            .as_array()
            .context("elements")?
            .iter()
            .any(|row| row["role"]["text"] == "spin button")
        {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Krita's new document dialog did not open"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
