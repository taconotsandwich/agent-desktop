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
BIN="$PREFIX/bin/agent-desktop"
# KWin matches QFileInfo(first-word-of-Exec).canonicalFilePath() against
# /proc/pid/exe, so Exec/TryExec MUST be absolute (bare names never match).
cat > "$APP_DIR/agent-desktop.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Agent Desktop MCP Server
GenericName=Desktop Automation Server
Comment=General Linux desktop-control MCP server (KDE + GNOME, Wayland + X11)
Icon=preferences-desktop
TryExec=$BIN
Exec=$BIN
NoDisplay=true
Categories=Utility;Accessibility;

# Authorize this binary to call the org.kde.KWin.ScreenShot2 D-Bus interface.
X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2
EOF
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$APP_DIR/" >/dev/null 2>&1 || true
command -v kbuildsycoca6 >/dev/null 2>&1 \
  && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
if [ -f "$UNIT_SRC" ]; then
  install -Dm644 "$UNIT_SRC" "$UNIT_DIR/agent-desktop.service"
  systemctl --user daemon-reload
fi
echo "installed. doctor: agent-desktop served over your MCP client, or stdio smoke-test tools/call doctor"
