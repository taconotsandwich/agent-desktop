---
name: agent-desktop
description: Install, configure, and use agent-desktop for Linux desktop automation through its persistent JavaScript MCP tools, including KDE and GNOME sessions and isolated KDE virtual seats.
---

# Agent Desktop

Use agent-desktop's MCP server to observe and operate installed Linux desktop applications. The npm distribution targets Linux x64 with glibc. Run it on the machine hosting the desktop session; a local MCP server cannot control an unrelated remote desktop.

## Installation and connection

The skill installs instructions only. It does not install the executable or register an MCP server. From a checkout, the Skills CLI can install this directory with `npx skills add . --skill agent-desktop`. Once the skill is available on the repository's default branch, use:

```sh
npx skills add taconotsandwich/agent-desktop --skill agent-desktop
```

The npm package is prepared for distribution but publication is deferred. Until published, use the project's locally packed npm artifacts and the installed `agent-desktop` command; do not assume a registry download will work. After publication:

```sh
npm install -g @taconotsandwich/agent-desktop
agent-desktop setup
agent-desktop doctor
```

The equivalent one-shot entrypoint is `npx @taconotsandwich/agent-desktop`. Rust is unnecessary for the prebuilt package, but native desktop dependencies remain necessary. Setup copies the executable to `${XDG_DATA_HOME:-$HOME/.local/share}/agent-desktop/versions/<version>/agent-desktop`, adds a launcher under `~/.local/bin` (override with `setup --bin-dir /absolute/path`), and installs desktop integration. Run it for the user's intended desktop and inspect its output before starting automation; installing the npm package alone does not grant desktop permissions.

`doctor --json` returns `{ok, version, checks}` without changing desktop configuration; exit status 1 means a required check failed. Use `--mode virtual doctor` to check virtual-seat prerequisites.

Configure the client's stdio MCP server using the installed launcher:

```json
{
  "mcpServers": {
    "agent-desktop": {
      "command": "agent-desktop",
      "args": []
    }
  }
}
```

Adapt the outer configuration syntax to the MCP client. For published npx use, set `command` to `npx` and `args` to `["--yes", "@taconotsandwich/agent-desktop"]`. Prefer an explicitly chosen package version for reproducibility. Do not send shell banners or application logs to the MCP stdout stream.

Live mode is the default. The server must inherit the actual session's `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, and display variables (`WAYLAND_DISPLAY` or `DISPLAY`, and `XAUTHORITY` where required). An SSH login usually does not inherit them. Discover the target session's values rather than hardcoding another user's runtime directory. `--session wayland|x11` and `--desktop kde|gnome|other` override detection; they do not create or attach a desktop session.

## Persistent JavaScript workflow

Find this server's `js` and `js_reset` MCP tools; client namespaces vary. Pass JavaScript as the `code` argument to `js`. The runtime provides `agentdesktop` and `nodeRepl`; it is not a Node shell and does not expose arbitrary filesystem or process APIs.

Start by observing:

```js
await agentdesktop.getState();
```

Discover installed application identities with `await agentdesktop.listApps()`. Select an observed application name, desktop ID, or `.desktop` file path:

```js
const app = await agentdesktop.getApp("org.kde.kate.desktop");
```

The example requires Kate to be installed. Selection emits initial state and can launch the application through its desktop entry. A raw executable path is not an application desktop entry. Resolve an ambiguous match with the exact ID returned by discovery.

Bindings persist between calls. Continue with the selected target:

```js
await app.getAXStateAndScreenshot();
```

Observations emit text and images automatically. Read them before choosing actions. Use element indices from the latest accessibility state or `[x, y]` coordinates from the latest target screenshot. Screenshot coordinates may be scaled; do not substitute desktop coordinates. After a mutation, observe again before reusing an element index.

Supported target operations include:

- `getAXState({disableDiffing: true})` for a complete accessibility observation.
- `getScreenshot()` and `getAXStateAndScreenshot()` for visual observation.
- `click(indexOrCoordinates, {mouseButton, clickCount})`, `drag([x, y], [x, y])`, and `scroll(indexOrCoordinates, direction, pages)`.
- `pressKey("ctrl+s")` for a key chord; `typeText(text)` and `paste(text)` for literal text.
- `setValue(elementIndex, value)`, `selectText(elementIndex, text, options)`, and `performSecondaryAction(elementIndex, action)` when supported by the observed element.

Use the connected tool's description for optional fields. Rich clipboard formats are unsupported. Coordinate input focuses the target; semantic actions can operate in the background only where the app supports them. If an action returns `action_pending`, observe before retrying to avoid applying it twice.

`nodeRepl.write(value)` emits text; `nodeRepl.emitImage(bytes)` emits a PNG. Observation methods accept `{emit: false}` if the result is needed without automatic output. Ordinary errors preserve bindings; a timeout resets them. After `js_reset` or a timeout, observe and select the app again. Resetting JavaScript does not close applications.

## Isolated sessions and cleanup

For an isolated managed KDE/Wayland desktop, set the MCP server arguments to `["--mode", "virtual"]`. This requires KWin, XWayland, D-Bus, and AT-SPI helpers on the host. It starts a private compositor and bus; it does not clone the user's running apps. Shut down the MCP connection cleanly when done so the server tears down its seat.

For multiple persistent seats:

```sh
agent-desktop farm status
agent-desktop farm up -n 2
```

Inspect the returned health and `env_file` for each seat. Start a separate MCP server per seat with `["--join-seat", "/actual/returned/session.env"]`. Do not guess the temporary path. Each seat has a control lock; do not run two controllers against the same seat. Joining a seat does not own its lifetime, and disconnecting that server leaves the farm running.

`agent-desktop farm down` stops the entire farm recorded under the current runtime directory. Use it only when this task owns that farm; first inspect `farm status` so existing shared seats are not stopped accidentally. Close applications and processes created for the task through their UI or tracked process ownership. There is no `agentdesktop.closeApp()` API, and `js_reset` is not process cleanup.

## Troubleshooting

- Run `agent-desktop doctor` in the same session environment as the failing server. Check its native dependency, bus, and desktop integration findings before retrying.
- KDE screenshot authorization is tied to the executable's absolute path. Use setup's stable installed executable instead of registering an npx cache path manually.
- KWin virtual screenshots can require a DRM render device even with CPU rendering. On a device-free host, QPainter fallback can cancel screenshots; inspect the compositor renderer before treating this as an npm installation failure.
- GNOME automation requires the supplied Shell extension, currently targeting GNOME Shell 49. Setup installs `agent-desktop@local`; log out and back in when instructed, then enable it with `gnome-extensions enable agent-desktop@local` in the user's session.
- Empty accessibility results can be app-specific. Observe a screenshot and check the app's accessibility support before concluding the desktop connection is broken.
- Treat permission failures as a configuration problem; do not keep replaying input or approve desktop permission dialogs on the user's behalf.

Perform only the user's intended desktop actions. Selecting an app or having access to a live session does not authorize unrelated messages, purchases, or settings changes.
