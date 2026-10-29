#!/usr/bin/python3
"""Input on the disposable KWin seat: abs:x,y, button:code,state, key:code,state."""

import os, socket, struct, sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(
  os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
)


def send(obj, op, payload=b""):
  sock.sendall(
    struct.pack("<II", obj, ((8 + len(payload)) << 16) | op) + payload
  )


def string(s):
  b = s.encode() + b"\0"
  return struct.pack("<I", len(b)) + b + b"\0" * ((-len(b)) % 4)


def fixed(v):
  return struct.pack("<i", int(round(v * 256)))


buf = b""


def events():
  global buf
  while True:
    while len(buf) >= 8:
      obj, so = struct.unpack("<II", buf[:8])
      size, op = so >> 16, so & 0xFFFF
      if len(buf) < size:
        break
      payload, buf = buf[8:size], buf[size:]
      yield obj, op, payload
    d = sock.recv(65536)
    if not d:
      raise SystemExit("disconnected")
    buf += d


next_id = 2


def new_id():
  global next_id
  i = next_id
  next_id += 1
  return i


def roundtrip(on_event=None):
  cb = new_id()
  send(1, 0, struct.pack("<I", cb))
  for obj, op, p in events():
    if obj == cb and op == 0:
      return
    if obj == 1 and op == 0:
      raise SystemExit(f"wayland error: {p!r}")
    if on_event:
      on_event(obj, op, p)


registry = new_id()
send(1, 1, struct.pack("<I", registry))
g = {}


def on_global(obj, op, p):
  if obj == registry and op == 0:
    name, n = struct.unpack("<II", p[:8])
    iface = p[8 : 8 + n - 1].decode()
    g[iface] = (name, struct.unpack("<I", p[8 + n + ((-n) % 4) :][:4])[0])


roundtrip(on_global)
if "org_kde_kwin_fake_input" not in g:
  raise SystemExit("no org_kde_kwin_fake_input")
name, version = g["org_kde_kwin_fake_input"]
fi = new_id()
send(
  registry,
  0,
  struct.pack("<I", name)
  + string("org_kde_kwin_fake_input")
  + struct.pack("<II", min(version, 4), fi),
)
send(fi, 0, string("linuxapk test") + string("pointer test"))
roundtrip()
actions = (
  (line.strip() for line in sys.stdin)
  if sys.argv[1:] == ["--stdin"]
  else sys.argv[1:]
)
for a in actions:
  k, _, v = a.partition(":")
  x, y = (float(t) for t in v.split(","))
  if k == "abs":
    send(fi, 9, fixed(x) + fixed(y))
  elif k in ("key", "button"):
    send(fi, 10 if k == "key" else 2, struct.pack("<II", int(x), int(y)))
  elif k == "axis":
    send(fi, 3, struct.pack("<I", int(x)) + fixed(y))
  roundtrip()
roundtrip()
