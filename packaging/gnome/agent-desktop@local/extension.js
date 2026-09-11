import Gio from "gi://Gio";
import GLib from "gi://GLib";
import Meta from "gi://Meta";
import Shell from "gi://Shell";
import { Extension } from "resource:///org/gnome/shell/extensions/extension.js";
import * as Main from "resource:///org/gnome/shell/ui/main.js";

const path = "/org/agentdesktop/Windows";
const iface = "org.agentdesktop.Windows";
const xml = `<node><interface name="org.agentdesktop.Windows">
<method name="Query"><arg type="s" direction="out"/></method>
<method name="Control"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="ai" direction="in"/></method>
<method name="Screenshot"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>`;

function windows() {
  return global
    .get_window_actors()
    .map((actor) => actor.meta_window)
    .filter(
      (window) =>
        !window.is_override_redirect() &&
        [
          Meta.WindowType.NORMAL,
          Meta.WindowType.DIALOG,
          Meta.WindowType.MODAL_DIALOG,
        ].includes(window.get_window_type()),
    );
}
function find(id) {
  const window = windows().find((window) => `gnome:${window.get_id()}` === id);
  if (!window) throw new Error("Window no longer exists");
  return window;
}
function focus(window) {
  if (
    global.display.focus_window === window &&
    !Main.overview.visible &&
    !window.minimized
  )
    return;
  Main.overview.hide();
  window.unminimize();
  Main.activateWindow(window);
}

class WindowService {
  Query() {
    const tracker = Shell.WindowTracker.get_default();
    return JSON.stringify(
      windows().map((window) => {
        const geometry = window.get_frame_rect();
        return {
          window_ref: `gnome:${window.get_id()}`,
          title: window.get_title() ?? "",
          class: window.get_wm_class() ?? "",
          app_id: tracker.get_window_app(window)?.get_id() ?? "",
          pid: window.get_pid() || null,
          minimized: window.minimized,
          client_protocol:
            window.get_client_type() === Meta.WindowClientType.WAYLAND
              ? "wayland"
              : "x11",
          geometry: {
            x: geometry.x,
            y: geometry.y,
            w: geometry.width,
            h: geometry.height,
          },
          screen: window.get_monitor(),
          is_active:
            global.display.focus_window === window && !Main.overview.visible,
        };
      }),
    );
  }
  Control(id, action, geometry) {
    const window = find(id);
    switch (action) {
      case "focus":
        focus(window);
        break;
      case "minimize":
        window.minimize();
        break;
      case "maximize":
        window.maximize();
        break;
      case "restore":
        window.unmaximize();
        window.unminimize();
        break;
      case "close":
        window.delete(global.get_current_time());
        break;
      case "move_resize":
        if (geometry.length !== 4 || geometry[2] <= 0 || geometry[3] <= 0)
          throw new Error("Invalid geometry");
        window.move_resize_frame(true, ...geometry);
        break;
      default:
        throw new Error("Unknown window operation");
    }
  }
  async ScreenshotAsync([id], invocation) {
    try {
      const window = find(id);
      focus(window);
      await new Promise((resolve) =>
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 350, () => {
          resolve();
          return GLib.SOURCE_REMOVE;
        }),
      );
      if (global.display.focus_window !== window)
        throw new Error("Target did not receive focus");
      const stream = Gio.MemoryOutputStream.new_resizable();
      const screenshot = new Shell.Screenshot();
      // Capture exactly the frame: window snapshots can include an
      // unreported shadow margin, which breaks screenshot coordinates.
      const rect = window.get_frame_rect();
      await new Promise((resolve, reject) =>
        screenshot.screenshot_area(
          rect.x,
          rect.y,
          rect.width,
          rect.height,
          stream,
          (source, result) => {
            try {
              const resultValue = source.screenshot_area_finish(result);
              const success = Array.isArray(resultValue)
                ? resultValue[0]
                : resultValue;
              if (!success)
                throw new Error("GNOME did not capture the target window");
              resolve();
            } catch (error) {
              reject(error);
            }
          },
        ),
      );
      stream.close(null);
      const bytes = stream.steal_as_bytes().get_data();
      if (bytes.length < 8)
        throw new Error(
          `GNOME returned an empty screenshot (${bytes.length} bytes)`,
        );
      invocation.return_value(
        new GLib.Variant("(s)", [GLib.base64_encode(bytes)]),
      );
    } catch (error) {
      invocation.return_dbus_error("org.agentdesktop.Error", String(error));
    }
  }
}
export default class AgentDesktop extends Extension {
  enable() {
    this.service = Gio.DBusExportedObject.wrapJSObject(
      xml,
      new WindowService(),
    );
    this.service.export(Gio.DBus.session, path);
    this.owner = Gio.DBus.session.own_name(
      iface,
      Gio.BusNameOwnerFlags.NONE,
      null,
      null,
    );
  }
  disable() {
    Gio.DBus.session.unown_name(this.owner);
    this.service.unexport();
    this.service = null;
  }
}
