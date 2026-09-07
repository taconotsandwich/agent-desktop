use agent_desktop::{core::RefStore, server::AgentDesktop};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "agent-desktop", about = "General Linux desktop-control MCP server")]
struct Cli {
    /// live | virtual (virtual spawns an isolated compositor; KDE first)
    #[arg(long, default_value = "live")]
    mode: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    tracing::info!(mode = %cli.mode, "agent-desktop starting (stdio)");
    let server = AgentDesktop::new(Arc::new(RefStore::new()));
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}
