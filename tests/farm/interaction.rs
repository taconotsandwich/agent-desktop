use super::{Farm, blender, mcp};
use anyhow::{Context, Result, ensure};
use std::{path::Path, time::Duration};

#[tokio::test]
#[ignore = "requires host KWin, D-Bus, AT-SPI, Konsole, and Blender"]
async fn parallel_seats_deliver_input_and_observe_independent_results() -> Result<()> {
    let farm = Farm::new()?;
    let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/qa")
        .join(format!("farm-interaction-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&artifacts)?;
    eprintln!("QA artifacts: {}", artifacts.display());
    let status = farm.json(&["farm", "up", "-n", "2"]).await?;
    let seats = status["seats"].as_array().context("farm seats")?;
    ensure!(seats.len() == 2);
    let a = seats[0]["env_file"].as_str().context("first seat")?;
    let b = seats[1]["env_file"].as_str().context("second seat")?;
    let first_artifacts = artifacts.join("seat-0");
    let second_artifacts = artifacts.join("seat-1");
    let (first, second) = tokio::join!(
        exercise(a, &first_artifacts, "Seat Zero", 2, &farm.owner),
        exercise(b, &second_artifacts, "Seat One", 7, &farm.owner),
    );
    let first = first?;
    let second = second?;
    ensure!(
        first != second,
        "seats must own different Blender processes"
    );
    ensure!(farm.json(&["farm", "down"]).await?["stopped"] == 2);
    ensure!(farm.members().is_empty(), "interaction test leaked helpers");
    Ok(())
}

async fn exercise(
    env: &str,
    artifacts: &Path,
    label: &str,
    offset: i32,
    owner: &str,
) -> Result<u64> {
    std::fs::create_dir_all(artifacts)?;
    let environment = agent_desktop::session::environment::read_environment(env)?;
    let applications =
        Path::new(environment.get("XDG_DATA_HOME").context("private data")?).join("applications");
    std::fs::create_dir_all(&applications)?;
    std::fs::write(
        applications.join("org.kde.konsole.desktop"),
        "[Desktop Entry]\nType=Application\nName=Konsole\nExec=konsole --separate --platform wayland\n",
    )?;
    let mut client = mcp::Joiner::spawn(
        Path::new(env!("CARGO_BIN_EXE_agent-desktop")),
        env,
        &artifacts.join("server.log"),
    )
    .await?;
    let desktop = client.json("await agentdesktop.getState();").await?;
    ensure!(
        desktop["capabilities"]["input"] == "kwin-eis",
        "QA must exercise KWin EIS input"
    );
    let native = client
        .json("const terminal = await agentdesktop.getApp('org.kde.konsole.desktop');")
        .await?;
    let native_pid = native["window"]["pid"].as_u64().context("Konsole PID")?;
    ensure!(
        native["window"]["client_protocol"] != "x11",
        "native Wayland incorrectly reported as X11"
    );
    ensure!(
        std::fs::read_to_string(format!("/proc/{native_pid}/maps"))?
            .contains("libQt6WaylandClient"),
        "Konsole launched with --platform wayland must load the Wayland client"
    );
    ensure!(
        !has_x11_window(&environment, native_pid).await?,
        "native Konsole appeared on XWayland"
    );
    let before = artifacts.join("native-before.png");
    client.screenshot("terminal", &before).await?;
    let marker = format!("INPUT {label}");
    client
        .eval(&format!(
            "await terminal.typeText({});",
            serde_json::to_string(&marker)?
        ))
        .await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state = mcp::save_state(
            &mut client,
            "terminal",
            &artifacts.join("native-after.json"),
        )
        .await?;
        if state["elements"].to_string().contains(&marker) {
            break;
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "native Wayland did not expose typed text: {state}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let after = artifacts.join("native-after.png");
    client.screenshot("terminal", &after).await?;
    let changed = mcp::changed_pixels(&before, &after)?;
    ensure!(changed > 50, "native input changed only {changed} pixels");
    eprintln!(
        "{label}: native Wayland typed text verified through AT-SPI; {changed} pixels changed"
    );
    client
        .eval("await terminal.pressKey('ctrl+u'); await terminal.pressKey('ctrl+d');")
        .await?;
    closed_capture(&mut client, "terminal").await?;

    let mut process = tokio::process::Command::new("blender")
        .envs(&environment)
        .env_remove("WAYLAND_DISPLAY")
        .env("AGENT_DESKTOP_FARM_QA", owner)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(std::fs::File::create(
            artifacts.join("blender.log"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    let launched_pid = process.id().context("launched Blender PID")? as u64;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let desktop = client.json("await agentdesktop.getState();").await?;
        if desktop["windows"]
            .as_array()
            .context("windows")?
            .iter()
            .any(|window| window["pid"] == launched_pid)
        {
            break;
        }
        ensure!(
            process.try_wait()?.is_none(),
            "Blender exited before mapping a window"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "Blender did not map its XWayland window"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    blender::workflow(&mut client, artifacts, &environment, label, offset).await?;
    let state = client.json("await app.getAXState();").await?;
    let pid = state["window"]["pid"].as_u64().context("Blender PID")?;
    ensure!(pid == launched_pid, "selected a different Blender process");
    ensure!(
        has_x11_window(&environment, pid).await?,
        "Blender must have a real XWayland window"
    );
    client.eval("await app.pressKey('ctrl+q');").await?;
    closed_capture(&mut client, "app").await?;
    tokio::time::timeout(Duration::from_secs(10), process.wait()).await??;
    eprintln!(
        "{label}: Blender {pid} moved, renamed, saved, reopened, and closed; stale captures rejected"
    );
    client.close().await?;
    Ok(pid)
}

async fn closed_capture(client: &mut mcp::Joiner, target: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let result = client.json(&format!("{{ let code = ''; try {{ await {target}.getScreenshot({{emit:false}}); }} catch(error) {{ code=error.code; }} nodeRepl.write(JSON.stringify(code)); }}")).await?;
        if result == "app_closed" {
            return Ok(());
        }
        ensure!(result == "", "unexpected close observation: {result}");
        ensure!(
            std::time::Instant::now() < deadline,
            "closed app still returns an image"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn has_x11_window(
    environment: &std::collections::BTreeMap<String, String>,
    pid: u64,
) -> Result<bool> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("xdotool")
            .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
            .envs(environment)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        matches!(output.status.code(), Some(0 | 1)),
        "cannot inspect the QA seat's XWayland clients"
    );
    Ok(output.status.success() && !output.stdout.is_empty())
}
