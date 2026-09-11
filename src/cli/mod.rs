mod serve;
use agent_desktop::{
    platform::drivers::{Desktop, SessionType},
    session::{environment::join_seat, farm, seat::VirtualSeat},
};
use clap::{Parser, Subcommand, ValueEnum};
use serve::serve;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    Live,
    Virtual,
}

#[derive(Debug, Parser)]
#[command(
    name = "agent-desktop",
    version,
    about = "Linux computer use with a persistent JavaScript interface"
)]
struct Cli {
    #[arg(long, value_enum, default_value = "live")]
    mode: Mode,
    #[arg(long, value_enum)]
    session: Option<SessionType>,
    #[arg(long, value_enum)]
    desktop: Option<Desktop>,
    #[arg(long)]
    join_seat: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    Farm {
        #[command(subcommand)]
        action: FarmAction,
    },
}
#[derive(Debug, Subcommand)]
enum FarmAction {
    Up {
        #[arg(short, long, default_value_t = 2)]
        n: u32,
        #[arg(long, default_value_t = 1800)]
        width: u32,
        #[arg(long, default_value_t = 1125)]
        height: u32,
    },
    Down,
    Status,
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    if cli.mode == Mode::Virtual && cli.join_seat.is_none() {
        anyhow::ensure!(
            cli.session
                .is_none_or(|session| session == SessionType::Wayland)
                && cli.desktop.is_none_or(|desktop| desktop == Desktop::Kde),
            "Managed virtual seats use KDE/Wayland; use --join-seat for an existing X11 or GNOME session"
        );
    }
    let bootstrap = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Some(Command::Farm { action }) = cli.command {
        return bootstrap.block_on(farm_cmd(action));
    }
    let seat = if cli.mode == Mode::Virtual && cli.join_seat.is_none() {
        Some(bootstrap.block_on(VirtualSeat::boot(1800, 1125))?)
    } else {
        None
    };
    drop(bootstrap);
    if let Some(path) = cli
        .join_seat
        .as_deref()
        .or_else(|| seat.as_ref().map(|seat| seat.env_file.as_str()))
    {
        join_seat(path)?;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is required"))?;
    let _control = agent_desktop::session::process::FileLock::acquire(
        &std::path::PathBuf::from(runtime_dir).join("agent-desktop-control.lock"),
    )?;
    let private_session = seat.is_some() || cli.join_seat.is_some();
    let result = runtime.block_on(serve(cli.session, cli.desktop, private_session));
    if let Some(seat) = seat {
        seat.shutdown();
    }
    result
}

async fn farm_cmd(action: FarmAction) -> anyhow::Result<()> {
    match action {
        FarmAction::Up { n, width, height } => println!(
            "{}",
            serde_json::to_string_pretty(&farm::up(n, width, height).await?)?
        ),
        FarmAction::Down => println!(
            "{}",
            serde_json::json!({"seats": [], "stopped": farm::down()?})
        ),
        FarmAction::Status => println!("{}", serde_json::to_string_pretty(&farm::status().await?)?),
    }
    Ok(())
}
