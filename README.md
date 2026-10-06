# agent-desktop

agent-desktop is an MCP server that lets an agent observe and operate the
applications of a Linux desktop. It exposes two tools, `js` and `js_reset`,
and a persistent JavaScript API called `agentdesktop` that selects an
application, reads its accessibility tree, takes screenshots and delivers
input. Installation, client configuration and the API walkthrough live in
[skills/agent-desktop/SKILL.md](skills/agent-desktop/SKILL.md); this page
documents what each supported desktop can and cannot do.

Four backends are supported:

- KDE Plasma on Wayland (KWin scripting, KWin screenshots, EIS input).
- KDE Plasma on X11 (EWMH window control, X11 screenshots, XTEST and
  SendEvent input).
- GNOME on Wayland (the bundled Shell extension, currently for Shell 49,
  plus Mutter remote desktop input).
- Virtual seats: private KDE Wayland compositors started with
  `--mode virtual` or `farm up`.

## Modes and what they reach

Live mode is the default. The server joins the user's running session and
works on the applications that are already open there, or launches the ones
it is asked for through their desktop entries.

`--mode virtual` and `agent-desktop farm up` start private compositors
instead. A seat never reaches an application that is open on the user's
desktop; it only sees the applications launched inside it. Input inside a
seat is delivered by the seat's own compositor, so it never disturbs the
user, and several seats can run in parallel for several agents.

## Capability matrix

Each cell says what an `agentdesktop` call does on that backend:

- `background`: the target window is not activated and the real pointer
  does not move.
- `on request`: background when the application was selected with
  `agentdesktop.getApp(query, {background: true})`, foreground otherwise.
- `foreground`: the window is activated first and the pointer is moved onto
  it. The user's focus and cursor change.
- `inside seat`: the action happens inside the private compositor, which
  the user never sees.
- `unsupported`: the call fails with an error.

| Function | KDE Wayland | KDE X11 | GNOME Wayland | Virtual or farm seat |
|---|---|---|---|---|
| `getState`, `listApps`, `getApp` of an open app | background | background | background | seat-launched apps only |
| `getApp` that launches the app | the new window takes focus (compositor policy) | same | same | inside seat |
| `getAXState` | background | background | background | background |
| `getScreenshot` | background (KWin renders the window itself) | background with a compositing manager; occluded regions are undefined without one | foreground (the extension activates the window before capturing) | background |
| `click(index)` on an element with an accessibility action | background | background | background | background |
| `click([x, y])`, `click(index)` without an action, right, middle and double clicks | foreground; on request for Xwayland windows | on request | foreground; on request for Xwayland windows | inside seat |
| `drag` | foreground; on request for Xwayland windows | on request; a drag the application turns into drag-and-drop may grab the real pointer | foreground; on request for Xwayland windows | inside seat |
| `scroll` | foreground; on request for Xwayland windows | on request; Qt applications ignore synthetic wheel events (see toolkit notes) | foreground; on request for Xwayland windows | inside seat |
| `pressKey` | foreground; on request for Xwayland windows | on request; the window is told it has focus for the keys, so toolkit shortcuts fire | foreground; on request for Xwayland windows | inside seat |
| `typeText` | foreground; on request for Xwayland windows | on request for characters the keyboard layout can produce; other text falls back to `paste` | foreground; on request for Xwayland windows | inside seat |
| `paste(text)` | foreground; the clipboard is replaced and restored 1.5 s later. Xwayland windows: on request, using the X11 selection | on request; the clipboard is replaced and restored | foreground, clipboard replaced and restored. Xwayland windows: on request, using the X11 selection | inside seat, private clipboard |
| `paste` with `format: "md"` or `"html"` | unsupported | unsupported | unsupported | unsupported |
| `setValue`, `selectText`, `performSecondaryAction` | background where the toolkit exposes them | same | same | same |
| Xwayland windows on a Wayland session | input over `DISPLAY` on request | not applicable | input over `DISPLAY` on request, best effort | same as KDE Wayland |

Background input on X11 and Xwayland is window-targeted: events are handed
to the client that owns the window with `XSendEvent`, so the window manager
never sees them, focus stays where it is and the pointer does not move.
This is the mechanism behind `xdotool --window`. On a Wayland session the
server connects to Xwayland through `DISPLAY` and matches the compositor's
window to the X11 client by process id and title; native Wayland windows
keep the foreground path.

Window-targeted input is opt-in per application, because each toolkit
decides what to do with synthetic events (see the toolkit notes) and an
application that mishandles them can fail without an error. Select the
application with `agentdesktop.getApp(query, {background: true})` to send
its clicks, drags, scrolls, keys and text to its window. Selecting it again
without the option returns it to the foreground path, and `js_reset`
clears the choice for every application.

Pasting into an Xwayland window serves the text on the X11 clipboard with
`xclip`, because an X11 client reads that selection and the compositor
only bridges the Wayland clipboard to X11 while an Xwayland window is
active. The Wayland clipboard stays untouched unless the user is working
in an Xwayland window at that moment, in which case the compositor bridges
the temporary selection and the restore.

Xwayland coordinates are assumed to match the compositor's logical
coordinates, which is the case when the compositor scales Xwayland
(Plasma's "Scaled by the system" setting and the GNOME default). The
"Apply scaling themselves" mode has not been verified.

## Why the Wayland rows stay foreground

A Wayland compositor has a single seat, and every input protocol it offers
(libei and EIS, the RemoteDesktop portal, virtual keyboard and pointer
protocols) emulates devices on that seat, so the compositor routes the
events to whichever window has focus. There is no protocol for addressing
an event to a window of the caller's choice, and neither KWin nor Mutter
exposes one over D-Bus. Delivering input to a background Wayland window
therefore needs a compositor patch, which is out of scope here; the
accessibility actions remain the background option on those backends.

## Toolkit notes for window-targeted input

Synthetic events are handed to the toolkit, and each toolkit decides what
to do with them. Chords press their modifier keys like a physical keyboard
and every event carries the modifier state, so toolkits that track the
modifier keys themselves (Blender) and toolkits that read the state field
(Qt) agree. Around each action the window is told that it has focus and
afterwards that it lost it again, so toolkits that act only in the focused
window behave as if it were active while the real focus stays where it is:

- Qt 5 and 6 accept synthetic key and button events. Text entry, clicks
  and shortcuts bound to actions and menus (`QAction`) work, because Qt
  believes the window focused for the duration of the action; without
  that, a click makes Qt ask the window manager to activate the window,
  which moves the real focus. Keys go to the widget the window last
  focused, so click a field before typing into it. Qt ignores synthetic
  wheel button events whenever the X server offers XInput 2.1 or later,
  which Xorg and Xwayland do, so `scroll` has no effect on Qt windows in
  the background.
- GTK 3 is expected to accept synthetic core events; this has not been
  verified here.
- GTK 4 handles only XInput 2 device events and is expected to ignore
  synthetic core events.
- Chromium, Electron and Firefox may ignore synthetic events, and
  applications that opt out of `send_event` input (xterm's `allowSendEvents`
  is off by default) drop them.
- Blender opened its file browser under window-targeted input in the QA
  suite but did not complete the save, for a reason not yet found, so the
  suite drives Blender in the foreground.
- A window that the window manager decorates receives input only inside
  its client area; points on the title bar or borders are rejected before
  anything is sent.

A window that a modal dialog of the same application blocks is refused
with `blocked_window`, because Qt answers focus events on a blocked window
by activating the dialog for real. The engine acts on the application's
active window, else on the window it last observed, so a dialog that opens
while the application is in the background is not selected until the user
or the window manager activates it; the blocked window takes input again
once the dialog is closed.

When targeted delivery does not reach an application, select it without
the option to use the foreground path, or use the accessibility actions;
inside a virtual seat the foreground path never disturbs the user.

## Compared with Codex Computer Use on macOS

Codex's computer use on macOS operates one application at a time without
taking it over from the user: it reads the accessibility tree and takes
screenshots of the chosen application without activating it, delivers
clicks and keystrokes to that application while the user keeps working
elsewhere, shows its own cursor as an overlay and a picture-in-picture
preview of the window it is driving, pastes text including markdown and
HTML while preserving the user's clipboard, reports accessibility diffs
between observations, asks for per-application permission, and lets
several agents drive different applications at the same time.

What agent-desktop matches today:

- Accessibility observation and accessibility actions run in the
  background on every backend, with diffs between observations.
- Screenshots run in the background on KDE and inside seats.
- Raw input runs in the background on X11 sessions and for Xwayland
  windows on Wayland sessions, for applications selected with
  `background: true`.

What it does not:

- There is no overlay cursor and no preview window.
- `paste` handles plain text only; markdown and HTML formats are rejected.
- Permission is per desktop session, not per application.
- On a live Wayland desktop, raw input to native windows is foreground and
  serialised by the engine, so several agents cannot share one desktop.
  Use farm seats for parallel agents.

## Reading capabilities at runtime

`agentdesktop.getState().capabilities` reports what the running server can
do:

- `coordinateInput` is `"foreground"`: an application selected without
  `background: true` has its window activated and the pointer moved for
  coordinate actions.
- `targetedInput` is the id of the window-targeted driver that applications
  selected with `background: true` use, currently `"x11-sendevent"`, or
  `null` when none probed successfully.
- `targetedClients` says which windows such an application can reach:
  `"all"` on X11, `"xwayland"` on a Wayland session with a reachable
  `DISPLAY`, or `null`.
- `screenshot`, `input` and `windows` name the drivers in use, and
  `accessibility` reports whether the AT-SPI bus is reachable.

Each window in `getState().windows` carries `client_protocol` when the
backend can tell: X11 sessions report `"x11"`, and the GNOME extension
reports `"x11"` for Xwayland windows and `"wayland"` for native ones.
KWin's scripting API does not report the client type of a window, so on
KDE Wayland sessions the field is `null`. On a Wayland session the driver
looks every window not reported as `"wayland"` up in the Xwayland client
list by process and title, per action. A window found there takes
background input when its application asked for it; any other takes the
foreground path.
