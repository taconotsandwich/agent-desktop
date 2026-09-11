use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct Joiner {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    identity: agent_desktop::session::process::ProcessIdentity,
    transcript: std::fs::File,
    ownership: String,
}
impl Joiner {
    pub async fn spawn(bin: &Path, env: &str, log: &Path) -> Result<Self> {
        let mut command = tokio::process::Command::new(bin);
        let ownership = uuid::Uuid::new_v4().to_string();
        command
            .arg("--join-seat")
            .arg(env)
            .env("AGENT_DESKTOP_QA_OWNER", &ownership)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(log)?))
            .kill_on_drop(true);
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let identity = agent_desktop::session::process::ProcessIdentity::read(
            child.id().context("server pid")? as i32,
        )
        .context("server exited during startup")?;
        let stdin = child.stdin.take().context("server stdin")?;
        let reader = BufReader::new(child.stdout.take().context("server stdout")?);
        let transcript = std::fs::File::create(log.with_extension("rpc.jsonl"))?;
        let mut client = Self {
            child,
            stdin: Some(stdin),
            reader,
            next_id: 0,
            identity,
            transcript,
            ownership,
        };
        client
            .rpc(
                "initialize",
                json!({"protocolVersion":"2025-03-26","capabilities":{},
            "clientInfo":{"name":"desktop-qa","version":"0.1.0"}}),
            )
            .await?;
        client
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        Ok(client)
    }
    async fn send(&mut self, message: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("server input closed")?;
        stdin.write_all(format!("{message}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        loop {
            let mut line = String::new();
            let bytes =
                tokio::time::timeout(Duration::from_secs(130), self.reader.read_line(&mut line))
                    .await??;
            if bytes == 0 {
                let status =
                    tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await??;
                anyhow::bail!("server exited before replying: {status}");
            }
            let message: Value = serde_json::from_str(&line)?;
            if message.get("id").is_none() {
                continue;
            }
            anyhow::ensure!(message["id"] == id, "unexpected response: {message}");
            anyhow::ensure!(message.get("error").is_none(), "RPC error: {message}");
            return Ok(message["result"].clone());
        }
    }
    pub async fn eval(&mut self, code: &str) -> Result<Vec<Value>> {
        let result = self
            .rpc(
                "tools/call",
                json!({"name":"js","arguments":{"code":code,"timeout_ms":120000}}),
            )
            .await?;
        {
            use std::io::Write;
            let mut recorded = result.clone();
            for item in recorded["content"].as_array_mut().into_iter().flatten() {
                if item["type"] == "image" {
                    item["data"] = json!("[image omitted]");
                }
            }
            writeln!(
                self.transcript,
                "{}",
                json!({"code":code,"result":recorded})
            )?;
        }
        if result["isError"] == true {
            let state=self.rpc("tools/call",json!({"name":"js","arguments":{"code":"await agentdesktop.getState();","timeout_ms":10000}})).await;
            use std::io::Write;
            writeln!(self.transcript, "{}", json!({"failureState":state.ok()}))?;
            anyhow::bail!("JavaScript error: {result}");
        }
        Ok(result["content"]
            .as_array()
            .context("content array")?
            .clone())
    }
    pub async fn json(&mut self, code: &str) -> Result<Value> {
        let items = self.eval(code).await?;
        let text = items
            .iter()
            .rev()
            .find_map(|item| item["text"].as_str())
            .context("text output")?;
        serde_json::from_str(text).context("JSON output")
    }
    pub async fn screenshot(&mut self, expression: &str, path: &Path) -> Result<(u32, u32)> {
        use base64::Engine as _;
        let items = self
            .eval(&format!("await {expression}.getScreenshot();"))
            .await?;
        let data = items
            .iter()
            .find_map(|item| item["data"].as_str())
            .context("screenshot")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        let image = image::load_from_memory(&bytes)?;
        std::fs::write(path, bytes)?;
        Ok((image.width(), image.height()))
    }
    pub async fn close(mut self) -> Result<()> {
        drop(self.stdin.take());
        tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await??;
        Ok(())
    }
}
impl Drop for Joiner {
    fn drop(&mut self) {
        for process in agent_desktop::session::process::ProcessIdentity::with_environment(
            "AGENT_DESKTOP_QA_OWNER",
            &self.ownership,
        ) {
            process.signal_process(libc::SIGKILL);
        }
        self.identity.signal(libc::SIGTERM);
        let _ = self.child.start_kill();
    }
}

pub async fn save_state(
    client: &mut Joiner,
    app: &str,
    path: &std::path::Path,
) -> Result<serde_json::Value> {
    let state = client.json(&format!("await {app}.getAXState();")).await?;
    std::fs::write(path, serde_json::to_vec_pretty(&state)?)?;
    Ok(state)
}

pub fn changed_pixels(before: &Path, after: &Path) -> Result<usize> {
    let before = image::open(before)?.to_rgb8();
    let after = image::open(after)?.to_rgb8();
    anyhow::ensure!(
        before.dimensions() == after.dimensions(),
        "comparison needs equal dimensions"
    );
    Ok(before
        .pixels()
        .zip(after.pixels())
        .filter(|(a, b)| a != b)
        .count())
}
