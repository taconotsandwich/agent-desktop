#!/usr/bin/env bash
# Fedora 43/44 KDE deps for agent-desktop build + test.
set -euo pipefail
sudo dnf install -y \
  gcc pkg-config \
  at-spi2-core at-spi2-atk \
  wl-clipboard \
  ydotool \
  spectacle grim \
  xdotool wmctrl xorg-x11-server-utils ImageMagick \
  wayland-devel libxkbcommon-devel
echo "uinput group:"
getent group input || echo "no input group"
echo "done. ensure: usermod -aG input \$USER + udev rule for /dev/uinput + ydotoold user service"
