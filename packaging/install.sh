#!/usr/bin/env bash
# Install agent-desktop: binary + .desktop authorization (KWin ScreenShot2)
# Run on the Linux host.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_SRC="$REPO/target/release/agent-desktop"
DESKTOP_SRC="$REPO/packaging/agent-desktop.desktop"
PREFIX="${PREFIX:-$HOME/.local}"
APP_DIR="$HOME/.local/share/applications"

cargo build --release --locked --manifest-path "$REPO/Cargo.toml"
install -Dm755 "$BIN_SRC" "$PREFIX/bin/agent-desktop"
BIN="$PREFIX/bin/agent-desktop"
# KWin matches QFileInfo(first-word-of-Exec).canonicalFilePath() against
# /proc/pid/exe, so Exec/TryExec MUST be absolute (bare names never match).
mkdir -p "$APP_DIR"
while IFS= read -r line; do
  case "$line" in
    TryExec=*|Exec=*) printf '%s=%s\n' "${line%%=*}" "$BIN" ;;
    *) printf '%s\n' "$line" ;;
  esac
done < "$DESKTOP_SRC" > "$APP_DIR/agent-desktop.desktop"
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$APP_DIR/" >/dev/null 2>&1 || true
command -v kbuildsycoca6 >/dev/null 2>&1 \
  && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
echo "Installed $BIN. Configure your MCP client to launch this host binary over stdio."
