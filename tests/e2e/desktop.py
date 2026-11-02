#!/usr/bin/python3
"""Fake desktop services for the e2e runner's isolated session bus."""

from pathlib import Path
import os
import sys

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
PATH = "/org/freedesktop/portal/desktop"
NOTIFY = "org.freedesktop.Notifications"
SETTINGS = "org.freedesktop.portal.Settings"
selected = Path(sys.argv[1])
selected.write_text("innkeeper-e2e-file")
settings = {"org.freedesktop.appearance": {"color-scheme": dbus.UInt32(0)}}


class Request(dbus.service.Object):
  @dbus.service.signal("org.freedesktop.portal.Request", signature="ua{sv}")
  def Response(self, code, results):
    pass


class OpenUri(dbus.service.Object):
  def respond(self, options, sender, results):
    path = (
      f"{PATH}/request/{sender[1:].replace('.', '_')}/{options['handle_token']}"
    )
    request = Request(bus, path)

    def answer():
      request.Response(0, results)
      request.remove_from_connection()
      return False

    GLib.idle_add(answer)
    return dbus.ObjectPath(path)

  @dbus.service.method(
    "org.freedesktop.portal.OpenURI",
    in_signature="ssa{sv}",
    out_signature="o",
    sender_keyword="sender",
  )
  def OpenURI(self, parent, uri, options, sender):
    print(f"openuri: {uri}", flush=True)
    return self.respond(options, sender, {})

  @dbus.service.method(
    "org.freedesktop.portal.OpenURI",
    in_signature="sha{sv}",
    out_signature="o",
    sender_keyword="sender",
  )
  def OpenFile(self, parent, fd, options, sender):
    fd = fd.take()
    print(
      f"openfile: {Path(os.readlink(f'/proc/self/fd/{fd}')).name}", flush=True
    )
    os.close(fd)
    return self.respond(options, sender, {})


# dbus-python finds a method by its Python name along the MRO, so FileChooser's
# OpenFile lives in a subclass of OpenURI's.
class Portal(OpenUri):
  @dbus.service.method(
    "org.freedesktop.portal.FileChooser",
    in_signature="ssa{sv}",
    out_signature="o",
    sender_keyword="sender",
  )
  def OpenFile(self, parent, title, options, sender):
    print(f"picker: {title}", flush=True)
    return self.respond(
      options, sender, {"uris": dbus.Array([selected.as_uri()], signature="s")}
    )

  # The portal's Settings. The desktop starts with no color scheme, and
  # SetSetting changes a value as a desktop's own settings would.
  @dbus.service.method(SETTINGS, in_signature="as", out_signature="a{sa{sv}}")
  def ReadAll(self, namespaces):
    return dbus.Dictionary(
      {
        ns: dbus.Dictionary(kv, signature="sv")
        for ns, kv in settings.items()
        if not namespaces or ns in namespaces
      },
      signature="sa{sv}",
    )

  @dbus.service.method(SETTINGS, in_signature="ss", out_signature="v")
  def ReadOne(self, namespace, key):
    return settings[namespace][key]

  @dbus.service.signal(SETTINGS, signature="ssv")
  def SettingChanged(self, namespace, key, value):
    pass

  @dbus.service.method(
    "dev.sargunv.Innkeeper.TestPortal", in_signature="ssv", out_signature=""
  )
  def SetSetting(self, namespace, key, value):
    settings.setdefault(namespace, {})[key] = value
    print(f"setting: {namespace} {key} {value}", flush=True)
    self.SettingChanged(namespace, key, value)


# The notification server. Each notification gets a new id, and Invoke
# invokes an action of the last one, as a click on its button would.
class Notifications(dbus.service.Object):
  last = 0

  @dbus.service.method(NOTIFY, in_signature="", out_signature="as")
  def GetCapabilities(self):
    return ["body", "actions"]

  @dbus.service.method(NOTIFY, in_signature="susssasa{sv}i", out_signature="u")
  def Notify(self, app, replaces, icon, title, body, actions, hints, timeout):
    print(f"notification: {title}: {body}", flush=True)
    if actions:
      print(f"notification actions: {' '.join(actions[1::2])}", flush=True)
    self.last = replaces or self.last + 1
    return self.last

  @dbus.service.signal(NOTIFY, signature="us")
  def ActionInvoked(self, id, key):
    pass

  @dbus.service.method(
    "dev.sargunv.Innkeeper.TestNotifications",
    in_signature="s",
    out_signature="",
  )
  def Invoke(self, key):
    self.ActionInvoked(self.last, key)

  @dbus.service.method(NOTIFY, in_signature="u", out_signature="")
  def CloseNotification(self, id):
    pass


names = [
  dbus.service.BusName(n, bus, do_not_queue=True)
  for n in ("org.freedesktop.portal.Desktop", NOTIFY)
]
portal = Portal(bus, PATH)
notifications = Notifications(bus, "/org/freedesktop/Notifications")
print("ready", flush=True)
GLib.MainLoop().run()
