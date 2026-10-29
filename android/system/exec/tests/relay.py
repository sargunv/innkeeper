"""The exec relay preserves exit status and cancels disconnected commands."""

import array
import os
import socket
import struct
import subprocess
import sys


def command(words):
  client, server = socket.socketpair()
  process = subprocess.Popen(
    [sys.argv[1], str(server.fileno())], pass_fds=[server.fileno()]
  )
  server.close()
  output, writer = os.pipe()
  null = os.open(os.devnull, os.O_RDWR)
  message = (
    b"\0".join(os.fsencode(w) for w in ["/", str(len(words)), *words]) + b"\0"
  )
  client.sendmsg(
    [struct.pack("=I", len(message))],
    [
      (
        socket.SOL_SOCKET,
        socket.SCM_RIGHTS,
        array.array("i", [null, writer, null]),
      )
    ],
  )
  client.sendall(message)
  os.close(null)
  os.close(writer)
  return client, process, os.fdopen(output)


for script, expected in [("exit 37", 37), ("kill -TERM $$", 143)]:
  client, process, output = command(["/bin/sh", "-c", script])
  with client, output:
    client.settimeout(5)
    assert (
      struct.unpack("=i", client.recv(4, socket.MSG_WAITALL))[0] == expected
    )
    assert process.wait(timeout=5) == 0

client, process, output = command(["/bin/sh", "-c", "echo $$; exec sleep 30"])
with output:
  pid = int(output.readline())
  client.close()
  assert process.wait(timeout=5) == 0
  try:
    os.kill(pid, 0)
  except ProcessLookupError:
    pass
  else:
    raise AssertionError("the command survived its disconnected client")
print("ok   exec status, signal status and disconnect cancellation")
