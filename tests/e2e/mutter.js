// The disposable GNOME Shell loads this through its --automation-script option.
import Clutter from "gi://Clutter";
import Gio from "gi://Gio";
import GLib from "gi://GLib";
import Shell from "gi://Shell";
import St from "gi://St";
import * as Main from "resource:///org/gnome/shell/ui/main.js";

let control;
let pointer;
let keyboard;

export function init() {
  console.log(
    `test desktop: automation init; Shell starting=${Main.layoutManager._startingUp}`,
  );
  St.Settings.get().inhibit_animations();
  // A new virtual pointer starts in the hot corner, which opens the overview.
  new Gio.Settings({ schema_id: "org.gnome.desktop.interface" }).set_boolean(
    "enable-hot-corners",
    false,
  );
  const seat = global.stage.context.get_backend().get_default_seat();
  pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
  keyboard = seat.create_virtual_device(
    Clutter.InputDeviceType.KEYBOARD_DEVICE,
  );
  // Shell connects startup-complete after it awaits this module's import. If
  // startup finished first, the performance runner never calls this module.
  if (!Main.layoutManager._startingUp) {
    console.log(
      "test desktop: Shell already started; starting automation directly",
    );
    run().catch(console.error);
  }
}

export async function run() {
  Main.overview.hide();
  control = Gio.DBusExportedObject.wrapJSObject(
    `<node><interface name="dev.sargunv.Innkeeper.TestDesktop">
      <method name="Input"><arg type="s" direction="in"/></method>
      <method name="Visibility"><arg type="b" direction="in"/><arg type="b" direction="out"/></method>
      <method name="Screenshot"><arg type="s" direction="in"/></method>
      <method name="Position"><arg type="b" direction="out"/></method>
      <method name="Geometry"><arg type="s" direction="in"/><arg type="(iiii)" direction="out"/></method>
      <method name="Close"><arg type="s" direction="in"/><arg type="b" direction="out"/></method>
      <method name="Layout"><arg type="s" direction="in"/></method>
    </interface></node>`,
    {
      async ScreenshotAsync([path], invocation) {
        try {
          const stream = Gio.File.new_for_path(path).replace(
            null,
            false,
            Gio.FileCreateFlags.REPLACE_DESTINATION,
            null,
          );
          try {
            await new Shell.Screenshot().screenshot(false, stream);
          } finally {
            stream.close(null);
          }
          invocation.return_value(null);
        } catch (error) {
          invocation.return_dbus_error(
            "dev.sargunv.innkeeper.Screenshot",
            String(error),
          );
        }
      },
      // Makes the layout the keyboard's xkb layout, and replies once Mutter
      // uses it.
      async LayoutAsync([layout], invocation) {
        const backend = global.backend;
        let id = 0;
        const changed = new Promise((resolve, reject) => {
          id = backend.connect("keymap-changed", resolve);
          GLib.timeout_add(GLib.PRIORITY_DEFAULT, 3000, () => {
            reject(new Error(`Mutter didn't load the layout ${layout}`));
            return GLib.SOURCE_REMOVE;
          });
        });
        // A failed set_keymap leaves nothing to await this promise.
        changed.catch(() => {});
        try {
          // Mutter 47 added the model argument.
          try {
            backend.set_keymap(layout, "", "", "");
          } catch {
            backend.set_keymap(layout, "", "");
          }
          await changed;
          invocation.return_value(null);
        } catch (error) {
          invocation.return_dbus_error(
            "dev.sargunv.innkeeper.Layout",
            String(error),
          );
        } finally {
          backend.disconnect(id);
        }
      },
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
            // The seat stream uses evdev codes, and Clutter numbers buttons
            // from 1.
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
      Visibility(visible) {
        let matched = false;
        for (const actor of global.get_window_actors()) {
          const window = actor.meta_window;
          if (
            (!visible && !actor.mapped) ||
            !String(window.get_wm_class()).includes(
              "dev.sargunv.innkeeper.probes",
            )
          )
            continue;
          matched = true;
          if (visible) {
            window.unminimize();
            window.activate(global.get_current_time());
          } else {
            window.minimize();
          }
        }
        return matched;
      },
      // Closes the windows whose class contains appId, as a close button
      // does.
      Close(appId) {
        let matched = false;
        for (const actor of global.get_window_actors()) {
          const window = actor.meta_window;
          if (!actor.mapped || !String(window.get_wm_class()).includes(appId))
            continue;
          window.delete(global.get_current_time());
          matched = true;
        }
        return matched;
      },
      Geometry(appId) {
        for (const actor of global.get_window_actors()) {
          const window = actor.meta_window;
          if (!actor.mapped || !String(window.get_wm_class()).includes(appId))
            continue;
          const r = window.get_frame_rect();
          return [r.x, r.y, r.width, r.height];
        }
        return [0, 0, 0, 0];
      },
      async PositionAsync(_args, invocation) {
        let positioned = 0;
        for (const actor of global.get_window_actors()) {
          if (!actor.mapped) continue;
          const window = actor.meta_window;
          if (
            !String(window.get_wm_class()).includes(
              "dev.sargunv.innkeeper.probes",
            )
          )
            continue;
          window.unminimize();
          window.move_frame(false, 300, 100);
          window.activate(global.get_current_time());
          positioned++;
        }
        await new Promise((resolve) => global.run_at_leisure(resolve));
        console.log(`test desktop: positioned ${positioned} windows`);
        invocation.return_value(new GLib.Variant("(b)", [positioned >= 1]));
      },
    },
  );
  control.export(Gio.DBus.session, "/dev/sargunv/Innkeeper/TestDesktop");
  Gio.bus_own_name_on_connection(
    Gio.DBus.session,
    "dev.sargunv.Innkeeper.TestDesktop",
    Gio.BusNameOwnerFlags.NONE,
    null,
    null,
  );
  console.log("test desktop: native input and window control ready");
  await new Promise(() => {});
}
