use std::path::PathBuf;

pub fn server_bin() -> PathBuf {
    let path = std::env::var_os("AGENT_DESKTOP_QA_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_agent-desktop")));
    assert!(path.is_file(), "QA executable missing: {}", path.display());
    path
}
