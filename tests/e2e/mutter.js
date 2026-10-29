// Loaded only by the disposable GNOME Shell's --automation-script option.
import Clutter from "gi://Clutter";
import Gio from "gi://Gio";
import GLib from "gi://GLib";
import St from "gi://St";
import * as Main from "resource:///org/gnome/shell/ui/main.js";

let control;
let pointer;
let keyboard;

export function init() {
  St.Settings.get().inhibit_animations();
  // A fresh virtual pointer starts in GNOME's overview hot corner.
  new Gio.Settings({ schema_id: "org.gnome.desktop.interface" }).set_boolean(
    "enable-hot-corners",
    false,
  );
  const seat = global.stage.context.get_backend().get_default_seat();
  pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
  keyboard = seat.create_virtual_device(
    Clutter.InputDeviceType.KEYBOARD_DEVICE,
  );
}

export async function run() {
  Main.overview.hide();
  control = Gio.DBusExportedObject.wrapJSObject(
    `<node><interface name="dev.linuxapk.TestDesktop">
      <method name="Input"><arg type="s" direction="in"/></method>
      <method name="Position"><arg type="b" direction="in"/></method>
    </interface></node>`,
    {
      Input(action) {
        const [kind, values] = action.split(":");
        const [x, y] = values.split(",").map(Number);
        const time = GLib.get_monotonic_time();
        switch (kind) {
          case "abs":
            pointer.notify_absolute_motion(time, x, y);
            break;
          case "rel":
            pointer.notify_relative_motion(time, x, y);
            break;
          case "button":
            // Clutter numbers buttons; the shared seat stream uses evdev codes.
            pointer.notify_button(
              time,
              x === 273 ? 3 : x === 274 ? 2 : x >= 275 ? x - 267 : x - 271,
              y,
            );
            break;
          case "key":
            keyboard.notify_key(time, x, y);
            break;
          case "axis":
            pointer.notify_scroll_continuous(
              time,
              x === 1 ? y : 0,
              x === 0 ? y : 0,
              Clutter.ScrollSource.WHEEL,
              Clutter.ScrollFinishFlags.NONE,
            );
            break;
          default:
            throw new Error(`Unknown test seat action: ${action}`);
        }
      },
      async PositionAsync([two], invocation) {
        let x = two ? 0 : 300;
        for (const actor of global.get_window_actors()) {
          const window = actor.meta_window;
          if (
            !String(window.get_wm_class()).includes(
              two ? "dev.linuxapk." : "dev.linuxapk.probes",
            )
          )
            continue;
          window.unminimize();
          if (two) window.move_resize_frame(false, x, 100, 800, 800);
          else {
            window.move_frame(false, x, 100);
            window.activate(global.get_current_time());
          }
          x += 1000;
        }
        await new Promise((resolve) => global.run_at_leisure(resolve));
        invocation.return_value(null);
      },
    },
  );
  control.export(Gio.DBus.session, "/dev/linuxapk/TestDesktop");
  Gio.bus_own_name_on_connection(
    Gio.DBus.session,
    "dev.linuxapk.TestDesktop",
    Gio.BusNameOwnerFlags.NONE,
    null,
    null,
  );
  await new Promise(() => {});
}
