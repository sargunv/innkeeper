#!/usr/bin/python3
"""Stand-ins for the host-state portals on a private session bus.

    mock-location-portal.py [--deny]

Owns org.freedesktop.portal.Desktop and serves org.freedesktop.portal.Location
(version 1): CreateSession, then Start answers its Request with Response 0, or
1 with --deny (the user said no). Fixes are sent with the mock method
dev.linuxapk.Mock.Fix(latitude, longitude, accuracy) on
/org/freedesktop/portal/desktop, as LocationUpdated on every started session.
Also serves NetworkMonitor v3 and PowerProfileMonitor v1; the mock methods
SetMetered and SetPowerSaver update their state and emit their change signals.
Only ever run it on a private bus (tests/hoststate/run.sh).
"""

import sys
import time

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

PATH = "/org/freedesktop/portal/desktop"
LOCATION = "org.freedesktop.portal.Location"
NETWORK = "org.freedesktop.portal.NetworkMonitor"
POWER = "org.freedesktop.portal.PowerProfileMonitor"
REQUEST = "org.freedesktop.portal.Request"
SESSION = "org.freedesktop.portal.Session"
DENY = "--deny" in sys.argv


class Request(dbus.service.Object):
  @dbus.service.signal(REQUEST, signature="ua{sv}")
  def Response(self, code, results):
    pass


class Session(dbus.service.Object):
  def __init__(self, bus, path, portal):
    super().__init__(bus, path)
    self.portal = portal

  @dbus.service.method(SESSION, in_signature="", out_signature="")
  def Close(self):
    print("session closed", self._object_path, flush=True)
    self.portal.started.discard(self._object_path)
    self.remove_from_connection()


class Portal(dbus.service.Object):
  def __init__(self, bus):
    super().__init__(bus, PATH)
    self.bus = bus
    self.started = set()
    self.keep = []
    self.metered = False
    self.saver = False

  @dbus.service.method(
    dbus.PROPERTIES_IFACE, in_signature="ss", out_signature="v"
  )
  def Get(self, iface, prop):
    props = self.GetAll(iface)
    if prop in props:
      return props[prop]
    raise dbus.exceptions.DBusException(
      "no such property", name="org.freedesktop.DBus.Error.UnknownProperty"
    )

  @dbus.service.method(
    dbus.PROPERTIES_IFACE, in_signature="s", out_signature="a{sv}"
  )
  def GetAll(self, iface):
    if iface == LOCATION:
      return {"version": dbus.UInt32(1)}
    if iface == NETWORK:
      return {"version": dbus.UInt32(3)}
    if iface == POWER:
      return {
        "version": dbus.UInt32(1),
        "power-saver-enabled": dbus.Boolean(self.saver),
      }
    return {}

  @dbus.service.method(NETWORK, in_signature="", out_signature="a{sv}")
  def GetStatus(self):
    return {
      "available": dbus.Boolean(True),
      "metered": dbus.Boolean(self.metered),
      "connectivity": dbus.UInt32(4),
    }

  @dbus.service.signal(NETWORK, signature="")
  def changed(self):
    pass

  @dbus.service.signal(dbus.PROPERTIES_IFACE, signature="sa{sv}as")
  def PropertiesChanged(self, iface, changed, invalidated):
    pass

  @dbus.service.method("dev.linuxapk.Mock", in_signature="b", out_signature="")
  def SetMetered(self, metered):
    self.metered = bool(metered)
    self.changed()

  @dbus.service.method("dev.linuxapk.Mock", in_signature="b", out_signature="")
  def SetPowerSaver(self, saver):
    self.saver = bool(saver)
    self.PropertiesChanged(
      POWER, {"power-saver-enabled": dbus.Boolean(self.saver)}, []
    )

  @dbus.service.method(
    LOCATION, in_signature="a{sv}", out_signature="o", sender_keyword="sender"
  )
  def CreateSession(self, options, sender):
    token = options.get("session_handle_token", "s")
    path = "%s/session/%s/%s" % (PATH, sender[1:].replace(".", "_"), token)
    self.keep.append(Session(self.bus, path, self))
    print("session", path, "accuracy", options.get("accuracy"), flush=True)
    return dbus.ObjectPath(path)

  @dbus.service.method(
    LOCATION, in_signature="osa{sv}", out_signature="o", sender_keyword="sender"
  )
  def Start(self, session, parent, options, sender):
    token = options.get("handle_token", "r")
    path = "%s/request/%s/%s" % (PATH, sender[1:].replace(".", "_"), token)
    req = Request(self.bus, path)
    self.keep.append(req)
    code = 1 if DENY else 0
    print("start", session, "->", code, flush=True)
    if code == 0:
      self.started.add(str(session))

    def answer():
      req.Response(dbus.UInt32(code), dbus.Dictionary({}, signature="sv"))
      return False

    GLib.timeout_add(200, answer)
    return dbus.ObjectPath(path)

  @dbus.service.signal(LOCATION, signature="oa{sv}")
  def LocationUpdated(self, session, location):
    pass

  @dbus.service.method(
    "dev.linuxapk.Mock", in_signature="ddd", out_signature="u"
  )
  def Fix(self, lat, lon, accuracy):
    now = time.time()
    loc = dbus.Dictionary(
      {
        "Latitude": dbus.Double(lat),
        "Longitude": dbus.Double(lon),
        "Accuracy": dbus.Double(accuracy),
        "Altitude": dbus.Double(-1.7976931348623157e308),
        "Speed": dbus.Double(-1),
        "Heading": dbus.Double(-1),
        "Timestamp": dbus.Struct(
          (dbus.UInt64(int(now)), dbus.UInt64(int(now % 1 * 1e6))),
          signature="tt",
        ),
      },
      signature="sv",
    )
    for s in self.started:
      self.LocationUpdated(dbus.ObjectPath(s), loc)
    print("fix", lat, lon, "to", len(self.started), "sessions", flush=True)
    return len(self.started)


DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus)
portal = Portal(bus)
print("ready", flush=True)
GLib.MainLoop().run()
