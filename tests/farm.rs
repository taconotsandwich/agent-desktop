use agent_desktop::session::process::ProcessIdentity;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Output, time::Duration};

#[path = "desktop/support/blender.rs"]
mod blender;
#[path = "farm/interaction.rs"]
mod interaction;
#[allow(dead_code)]
#[path = "desktop/support/mcp.rs"]
mod mcp;

struct Farm {
    directory: tempfile::TempDir,
    owner: String,
    path: Option<std::ffi::OsString>,
}

impl Farm {
    fn new() -> Result<Self> {
        Ok(Self {
            directory: tempfile::Builder::new()
                .prefix("agent-desktop-farm-qa-")
                .tempdir()?,
            owner: uuid::Uuid::new_v4().to_string(),
            path: None,
        })
    }

    async fn command(&self, args: &[&str]) -> Result<Output> {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-desktop"));
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", self.directory.path())
            .env("AGENT_DESKTOP_FARM_QA", &self.owner)
            .kill_on_drop(true);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        Ok(tokio::time::timeout(Duration::from_secs(60), command.output()).await??)
    }

    async fn json(&self, args: &[&str]) -> Result<Value> {
        let output = self.command(args).await?;
        ensure!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn members(&self) -> Vec<ProcessIdentity> {
        ProcessIdentity::with_environment("AGENT_DESKTOP_FARM_QA", &self.owner)
    }
}

impl Drop for Farm {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_agent-desktop"))
            .args(["farm", "down"])
            .env("XDG_RUNTIME_DIR", self.directory.path())
            .output();
        for process in self.members() {
            process.signal_process(libc::SIGKILL);
        }
    }
}

#[tokio::test]
#[ignore = "requires host KWin, D-Bus, and AT-SPI"]
async fn independent_farms_report_crashes_and_only_reap_their_own_seats() -> Result<()> {
    let first = Farm::new()?;
    let second = Farm::new()?;
    let first_up = first.json(&["farm", "up", "-n", "1"]).await?;
    let second_up = second.json(&["farm", "up", "-n", "1"]).await?;
    let a = &first_up["seats"][0];
    let b = &second_up["seats"][0];
    ensure!(a["health"] == "ready" && b["health"] == "ready");
    ensure!(a["runtime_dir"] != b["runtime_dir"]);
    ensure!(a["env_file"] != b["env_file"]);
    ensure!(
        a["wayland_display"] == b["wayland_display"],
        "same socket basename is isolated by private runtime directories"
    );
    let again = second.json(&["farm", "up", "-n", "1"]).await?;
    ensure!(
        again["seats"][0]["processes"] == b["processes"],
        "up reuses only a healthy seat"
    );

    let processes: Vec<ProcessIdentity> = serde_json::from_value(b["processes"].clone())?;
    let compositor = processes
        .iter()
        .find(|process| {
            std::fs::read_to_string(format!("/proc/{}/comm", process.pid))
                .is_ok_and(|name| name.trim() == "kwin_wayland")
        })
        .context("test-owned KWin process")?;
    compositor.signal_process(libc::SIGTERM);
    tokio::time::timeout(Duration::from_secs(5), async {
        while compositor.alive() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    let degraded = second.json(&["farm", "status"]).await?;
    ensure!(degraded["seats"][0]["health"] == "degraded", "{degraded}");
    ensure!(
        degraded["seats"][0]["dead_pids"]
            .as_array()
            .context("dead pids")?
            .contains(&Value::from(compositor.pid))
    );
    ensure!(
        !second
            .command(&["farm", "up", "-n", "1"])
            .await?
            .status
            .success()
    );
    ensure!(second.json(&["farm", "down"]).await?["stopped"] == 1);
    ensure!(second.json(&["farm", "status"]).await? == serde_json::json!({"seats": []}));
    ensure!(second.members().is_empty(), "stopped farm leaked helpers");
    ensure!(
        first.json(&["farm", "status"]).await?["seats"][0]["health"] == "ready",
        "other farm must survive"
    );
    first.json(&["farm", "down"]).await?;
    ensure!(first.members().is_empty(), "first farm leaked helpers");
    Ok(())
}

#[tokio::test]
#[ignore = "requires a host D-Bus daemon"]
async fn failed_compositor_startup_returns_failure_and_cleans_helpers() -> Result<()> {
    let mut farm = Farm::new()?;
    let bin = farm.directory.path().join("bin");
    std::fs::create_dir(&bin)?;
    let kwin = bin.join("kwin_wayland");
    std::fs::write(&kwin, "#!/bin/sh\nexit 23\n")?;
    std::fs::set_permissions(&kwin, std::fs::Permissions::from_mode(0o700))?;
    let mut paths = vec![PathBuf::from(&bin)];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    farm.path = Some(std::env::join_paths(paths)?);
    let result = farm.command(&["farm", "up", "-n", "1"]).await?;
    ensure!(
        !result.status.success(),
        "crashed compositor cannot produce a successful farm up"
    );
    ensure!(farm.json(&["farm", "status"]).await? == serde_json::json!({"seats": []}));
    ensure!(farm.members().is_empty(), "failed startup leaked helpers");
    Ok(())
}

#[test]
fn cli_reports_package_version() -> Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-desktop"))
        .arg("--version")
        .output()?;
    ensure!(output.status.success());
    ensure!(
        String::from_utf8(output.stdout)?.trim()
            == concat!("agent-desktop ", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires host KWin, D-Bus, AT-SPI, and Blender"]
async fn farm_servers_capture_blender_without_qa_permission_setup() -> Result<()> {
    let farm = Farm::new()?;
    let artifacts = std::env::var_os("AGENT_DESKTOP_QA_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/qa"))
        .join(format!("farm-screenshots-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&artifacts)?;
    eprintln!("QA artifacts: {}", artifacts.display());
    let status = farm.json(&["farm", "up", "-n", "2"]).await?;
    let relocated = farm.directory.path().join("server path/agent-desktop");
    std::fs::create_dir_all(relocated.parent().context("server directory")?)?;
    std::fs::copy(env!("CARGO_BIN_EXE_agent-desktop"), &relocated)?;
    let mut pids = Vec::new();
    for (index, seat) in status["seats"]
        .as_array()
        .context("farm seats")?
        .iter()
        .enumerate()
    {
        let mut client = mcp::Joiner::spawn(
            if index == 0 {
                std::path::Path::new(env!("CARGO_BIN_EXE_agent-desktop"))
            } else {
                &relocated
            },
            seat["env_file"].as_str().context("seat environment")?,
            &artifacts.join(format!("seat-{index}.log")),
        )
        .await?;
        let state = client
            .json("const app = await agentdesktop.getApp('blender.desktop');")
            .await?;
        let pid = state["window"]["pid"].as_u64().context("Blender PID")?;
        ensure!(
            !pids.contains(&pid),
            "each seat must own a different Blender process"
        );
        pids.push(pid);
        eprintln!(
            "seat {index}: Blender {pid}, protocol {}",
            state["window"]["client_protocol"]
        );
        let path = artifacts.join(format!("seat-{index}-before.png"));
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let (width, height) = client.screenshot("app", &path).await?;
            ensure!(width > 100 && height > 100);
            let image = image::open(&path)?.to_rgb8();
            let visible = image
                .pixels()
                .filter(|pixel| pixel.0.iter().any(|value| *value > 24))
                .count();
            if visible > (width as usize * height as usize) / 10 {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Blender did not paint a visible frame in the farm screenshot"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        client.eval("await app.pressKey('Escape');").await?;
        client
            .screenshot("app", &artifacts.join(format!("seat-{index}-after.png")))
            .await?;
        let fresh = client.json("await app.getAXState();").await?;
        ensure!(fresh["window"]["pid"] == pid);
        let frames = client.eval("for (let shot = 0; shot < 4; shot++) { await app.getScreenshot(); await app.pressKey('Escape'); } nodeRepl.write('captures-complete');").await?;
        ensure!(
            frames.iter().filter(|item| item["type"] == "image").count() == 4,
            "all four screenshots must arrive in one MCP response"
        );
        ensure!(
            frames
                .last()
                .is_some_and(|item| item["text"] == "captures-complete")
        );
        ensure!(
            client.json("await app.getAXState();").await?["window"]["pid"] == pid,
            "server must remain usable after a multi-capture call"
        );
        client.close().await?;
    }
    ensure!(farm.json(&["farm", "down"]).await?["stopped"] == 2);
    ensure!(
        farm.members().is_empty(),
        "farm screenshot test leaked helpers"
    );
    Ok(())
}
