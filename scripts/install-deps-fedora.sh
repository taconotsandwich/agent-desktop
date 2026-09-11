#!/usr/bin/env bash
# Fedora host build and runtime dependencies.
set -euo pipefail
sudo dnf install -y \
  gcc pkg-config \
  at-spi2-core at-spi2-atk \
  dbus-daemon dbus-tools \
  wl-clipboard xclip \
  maim xdotool xmodmap wmctrl xorg-x11-server-utils ImageMagick \
  wayland-devel libxkbcommon-devel
echo "Host dependencies installed."
