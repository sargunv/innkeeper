#!/usr/bin/python3
"""Synthetic host evdev sources, without uinput or access to real input devices.

serve DIR: two independently hotpluggable sources. Removing DIR/eventN closes
that source; SIGUSR1 removes controller 0, SIGUSR2 reconnects it. Each source's
FF socket records uploads, erases and playback in DIR/rumbleN.jsonl.
check DUMP: the input group's single Android gamepad enumeration assertion.
"""

import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import struct
import sys
import time


def check(path):
  text = Path(path).read_text()
  devices = re.findall(
    r"Device (-?\d+): Linux\.apk Gamepad [^\n]+\n(.*?)(?=\n  Device |\Z)",
    text,
    re.S,
  )
  ids = set()
  for device, body in devices:
    sources = re.search(r"Sources: ([^\n]+)", body)
    if sources and "GAMEPAD" in sources[1] and "JOYSTICK" in sources[1]:
      ids.add(device)
  assert len(ids) == 2, f"expected two distinct Android gamepads, found {ids}"
  print(
    f"ok   synthetic host controllers enumerate as Android gamepads: {sorted(ids)}"
  )


def serve(directory):
  directory.mkdir(parents=True, exist_ok=True)
  selector = selectors.DefaultSelector()
  listeners = {}
  clients = {}
  next_effect = [0, 0]

  def create(index):
    for suffix in ("", ".ff"):
      path = directory / f"event{index}{suffix}"
      path.unlink(missing_ok=True)
      listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
      listener.bind(str(path))
      listener.listen()
      listener.setblocking(False)
      listeners[(index, suffix)] = listener
      selector.register(
        listener, selectors.EVENT_READ, ("accept", index, suffix)
      )

  def remove(index):
    for key, sock in list(clients.items()):
      if key[0] == index:
        selector.unregister(sock)
        sock.close()
        del clients[key]
    for key, sock in list(listeners.items()):
      if key[0] == index:
        selector.unregister(sock)
        sock.close()
        del listeners[key]
    for suffix in ("", ".ff"):
      (directory / f"event{index}{suffix}").unlink(missing_ok=True)

  signal.signal(signal.SIGUSR1, lambda *_: remove(0))
  signal.signal(signal.SIGUSR2, lambda *_: create(0))
  create(0)
  create(1)
  (directory / "ready").write_text(str(os.getpid()))
  while True:
    for selected, _ in selector.select(0.05):
      mode, index, suffix = selected.data
      if mode == "accept":
        client, _ = selected.fileobj.accept()
        client.setblocking(False)
        key = (index, suffix)
        if key in clients:
          selector.unregister(clients[key])
          clients[key].close()
        clients[key] = client
        selector.register(
          client, selectors.EVENT_READ, ("client", index, suffix)
        )
        if suffix == "":
          micros = time.monotonic_ns() // 1000
          event = lambda ty, code, value: struct.pack(
            "qqHHi", micros // 1000000, micros % 1000000, ty, code, value
          )
          try:
            client.send(event(3, 0, 12345) + event(1, 304, 1) + event(0, 0, 0))
            client.send(event(1, 304, 0) + event(0, 0, 0))
          except BrokenPipeError:
            # The bridge retries registration until the broker is listening.
            selector.unregister(client)
            client.close()
            clients.pop(key, None)
      else:
        client = selected.fileobj
        data = client.recv(4096)
        if not data:
          selector.unregister(client)
          client.close()
          clients.pop((index, suffix), None)
          continue
        cmd = struct.unpack_from("I", data)[0]
        result = 0
        payload = bytearray(data[12:])
        if cmd & 255 == 0x80:
          effect = struct.unpack_from("h", payload, 2)[0]
          if effect == -1:
            effect = next_effect[index]
            next_effect[index] += 1
          struct.pack_into("h", payload, 2, effect)
        elif cmd & 255 == 0x84:
          payload = struct.pack("i", 16)
        with (directory / f"rumble{index}.jsonl").open("a") as log:
          log.write(json.dumps({"cmd": cmd, "bytes": data.hex()}) + "\n")
        if cmd:
          client.send(struct.pack("i", result) + payload)


if __name__ == "__main__":
  if sys.argv[1] == "check":
    check(sys.argv[2])
  else:
    serve(Path(sys.argv[2]))
