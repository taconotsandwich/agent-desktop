#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != --inside ]]; then
    exec dbus-run-session -- /bin/bash "$0" --inside "$@"
fi
shift
export XDG_RUNTIME_DIR
XDG_RUNTIME_DIR=$(mktemp -d /tmp/agent-desktop-qa.XXXXXX)
export XDG_CONFIG_HOME="$XDG_RUNTIME_DIR/config"
export XDG_CACHE_HOME="$XDG_RUNTIME_DIR/cache"
export XDG_DATA_HOME="$XDG_RUNTIME_DIR/data"
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
export GTK_A11Y=atspi
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export XDG_SESSION_TYPE="${QA_PROTOCOL:?}"
export XDG_CURRENT_DESKTOP="${QA_DESKTOP:?}"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" /artifacts
processes=()
cleanup() {
    local status=$?
    for pid in "${processes[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait || true
    if ! cp -a "$XDG_RUNTIME_DIR" /artifacts/session; then
        printf '%s\n' 'Session snapshot incomplete: runtime files changed during shutdown' >&2
    fi
    rm -rf "$XDG_RUNTIME_DIR"
    return "$status"
}
trap cleanup EXIT
dbus-daemon --session --nofork --print-address=1 > "$XDG_RUNTIME_DIR/system-bus.address" 2> /artifacts/system-bus.log &
processes+=("$!")
for attempt in {1..100}; do [[ -s "$XDG_RUNTIME_DIR/system-bus.address" ]] && break; sleep 0.05; done
export DBUS_SYSTEM_BUS_ADDRESS
read -r DBUS_SYSTEM_BUS_ADDRESS < "$XDG_RUNTIME_DIR/system-bus.address"
dbus-update-activation-environment XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_SESSION_TYPE XDG_CURRENT_DESKTOP DBUS_SYSTEM_BUS_ADDRESS
export AGENT_DESKTOP_QA_ARTIFACTS=/artifacts
rpm -qa > /artifacts/packages.txt
if [[ ${QA_SUITE:-desktop} == farm ]]; then
    cargo test --locked --test farm -- --ignored --nocapture --test-threads=1
    exit
fi
/usr/libexec/at-spi-bus-launcher --launch-immediately --a11y=1 --screen-reader=1 > /artifacts/atspi-bus.log 2>&1 &
processes+=("$!")
for attempt in {1..100}; do
    if address=$(gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress 2>/dev/null); then break; fi
    sleep 0.05
done
export AT_SPI_BUS_ADDRESS
AT_SPI_BUS_ADDRESS=$(printf '%s' "$address" | cut -d "'" -f 2)
/usr/libexec/at-spi2-registryd > /artifacts/atspi-registry.log 2>&1 &
processes+=("$!")
if [[ "$QA_PROTOCOL" == x11 ]]; then
    export DISPLAY=:91
    Xvfb "$DISPLAY" -screen 0 1800x1125x24 -nolisten tcp > /artifacts/xvfb.log 2>&1 &
    processes+=("$!")
    for attempt in {1..200}; do [[ -S /tmp/.X11-unix/X91 ]] && break; sleep 0.05; done
    if [[ "$QA_DESKTOP" == KDE ]]; then
        kwin_x11 --replace > /artifacts/compositor.log 2>&1 &
    else
        gnome-shell --x11 --replace > /artifacts/compositor.log 2>&1 &
    fi
    processes+=("$!")
else
    export WAYLAND_DISPLAY=wayland-qa
    if [[ "$QA_DESKTOP" == KDE ]]; then
        cat > "$XDG_RUNTIME_DIR/session-helper" <<'SESSION'
#!/bin/sh
printf 'DISPLAY=%s\nXAUTHORITY=%s\n' "$DISPLAY" "$XAUTHORITY" > "$XDG_RUNTIME_DIR/xwayland.env"
exec plasmashell
SESSION
        chmod 700 "$XDG_RUNTIME_DIR/session-helper"
        kwin_wayland --virtual --width 1800 --height 1125 --socket "$WAYLAND_DISPLAY" --xwayland --no-lockscreen --no-kactivities --exit-with-session "$XDG_RUNTIME_DIR/session-helper" > /artifacts/compositor.log 2>&1 &
    else
        export DISPLAY=:0
        mkdir -p "$XDG_DATA_HOME/gnome-shell/extensions"
        cp -a /work/packaging/gnome/agent-desktop@local "$XDG_DATA_HOME/gnome-shell/extensions/"
        gsettings set org.gnome.shell enabled-extensions "['agent-desktop@local']"
        gnome-shell --headless --wayland --wayland-display="$WAYLAND_DISPLAY" --virtual-monitor=1800x1125 > /artifacts/compositor.log 2>&1 &
    fi
    processes+=("$!")
    for attempt in {1..400}; do [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] && break; sleep 0.05; done
fi
if [[ "$QA_PROTOCOL" == x11 ]]; then
    for attempt in {1..200}; do
        if wmctrl -m > /artifacts/window-manager.txt 2>/dev/null; then break; fi
        sleep 0.05
    done
    wmctrl -m > /artifacts/window-manager.txt
elif [[ "$QA_DESKTOP" == KDE ]]; then
    for attempt in {1..200}; do
        if [[ -s "$XDG_RUNTIME_DIR/xwayland.env" ]] && gdbus introspect --session --dest org.kde.KWin --object-path /KWin > /artifacts/kwin.txt 2>/dev/null; then break; fi
        sleep 0.05
    done
    gdbus introspect --session --dest org.kde.KWin --object-path /KWin > /artifacts/kwin.txt
    set -a
    source "$XDG_RUNTIME_DIR/xwayland.env"
    set +a
    [[ -n "$DISPLAY" ]]
    if [[ -z "$XAUTHORITY" ]]; then unset XAUTHORITY; fi
    xprop -root > /artifacts/xwayland-root.txt
    gdbus call --session --dest org.kde.KWin --object-path /KWin --method org.kde.KWin.supportInformation > /artifacts/kwin-support.txt
fi
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY AT_SPI_BUS_ADDRESS
if [[ "$QA_DESKTOP" == GNOME ]]; then
    gsettings set org.gnome.shell disable-user-extensions false
    for attempt in {1..200}; do
        if gdbus call --session --dest org.agentdesktop.Windows --object-path /org/agentdesktop/Windows --method org.agentdesktop.Windows.Query > /artifacts/extension-ready.json 2>/dev/null; then break; fi
        sleep 0.05
    done
    gdbus introspect --session --dest org.gnome.Mutter.RemoteDesktop --object-path /org/gnome/Mutter/RemoteDesktop > /artifacts/mutter-remote-desktop.txt
    gjs -m /work/qa/containers/mutter-probe.js > /artifacts/mutter-session.xml
    gnome-extensions info agent-desktop@local > /artifacts/extension-info.txt
    shopt -s nullglob
    auth_files=("$XDG_RUNTIME_DIR"/.mutter-Xwaylandauth.*)
    if (( ${#auth_files[@]} )); then export XAUTHORITY="${auth_files[0]}"; fi
fi
if [[ "$QA_DESKTOP" == KDE ]]; then
    /usr/libexec/xdg-desktop-portal-kde > /artifacts/portal-backend.log 2>&1 &
else
    /usr/libexec/xdg-desktop-portal-gnome > /artifacts/portal-backend.log 2>&1 &
fi
processes+=("$!")
/usr/libexec/xdg-desktop-portal > /artifacts/portal.log 2>&1 &
processes+=("$!")
export AGENT_DESKTOP_QA_SESSION="$XDG_RUNTIME_DIR/session.env"
for key in DBUS_SESSION_BUS_ADDRESS DBUS_SYSTEM_BUS_ADDRESS AT_SPI_BUS_ADDRESS XAUTHORITY XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_SESSION_TYPE XDG_CURRENT_DESKTOP QT_LINUX_ACCESSIBILITY_ALWAYS_ON GTK_A11Y DISPLAY WAYLAND_DISPLAY; do
    [[ -v "$key" ]] && printf '%s=%s\n' "$key" "${!key}" >> "$AGENT_DESKTOP_QA_SESSION"
done
if [[ -f "$XDG_RUNTIME_DIR/xwayland.env" ]]; then cat "$XDG_RUNTIME_DIR/xwayland.env" >> "$AGENT_DESKTOP_QA_SESSION"; fi
cargo test --locked --test desktop -- --ignored --nocapture --test-threads=1
