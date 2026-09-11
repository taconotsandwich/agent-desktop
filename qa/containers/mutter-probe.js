import Gio from "gi://Gio";
import GLib from "gi://GLib";

const bus = Gio.DBus.session;
const destination = "org.gnome.Mutter.RemoteDesktop";
const [path] = bus
  .call_sync(
    destination,
    "/org/gnome/Mutter/RemoteDesktop",
    destination,
    "CreateSession",
    null,
    new GLib.VariantType("(o)"),
    Gio.DBusCallFlags.NONE,
    3000,
    null,
  )
  .deep_unpack();
const [xml] = bus
  .call_sync(
    destination,
    path,
    "org.freedesktop.DBus.Introspectable",
    "Introspect",
    null,
    new GLib.VariantType("(s)"),
    Gio.DBusCallFlags.NONE,
    3000,
    null,
  )
  .deep_unpack();
print(xml);
