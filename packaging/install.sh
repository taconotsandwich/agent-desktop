#!/usr/bin/env bash
# Install agent-desktop: binary + .desktop authorization (KWin ScreenShot2)
# + systemd-user unit. Run on the target host.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_SRC="$REPO/target/release/agent-desktop"
DESKTOP_SRC="$REPO/packaging/agent-desktop.desktop"
UNIT_SRC="$REPO/packaging/agent-desktop.service"
PREFIX="${PREFIX:-$HOME/.local}"
APP_DIR="$HOME/.local/share/applications"
UNIT_DIR="$HOME/.config/systemd/user"

cargo build --release --manifest-path "$REPO/Cargo.toml"
install -Dm755 "$BIN_SRC" "$PREFIX/bin/agent-desktop"
install -Dm644 "$DESKTOP_SRC" "$APP_DIR/agent-desktop.desktop"
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$APP_DIR/" >/dev/null 2>&1 || true
command -v kbuildsycoca6 >/dev/null 2>&1 \
  && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
if [ -f "$UNIT_SRC" ]; then
  install -Dm644 "$UNIT_SRC" "$UNIT_DIR/agent-desktop.service"
  systemctl --user daemon-reload
fi
echo "installed. doctor: agent-desktop served over your MCP client, or stdio smoke-test tools/call doctor"
