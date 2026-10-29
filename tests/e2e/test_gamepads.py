"""The synthetic controller source survives bridge registration retries."""

from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest

SOURCE = Path(__file__).with_name("gamepads.py")
OUTPUT = SOURCE.parents[2] / "out/test"


class GamepadSourceTest(unittest.TestCase):
  def test_unread_disconnect_allows_both_controllers_to_reconnect(self):
    OUTPUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gp-", dir=OUTPUT) as directory:
      directory = Path(directory)
      server = subprocess.Popen(
        [sys.executable, "-B", str(SOURCE), "serve", str(directory)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
      )
      try:
        end = time.monotonic() + 2
        while not (directory / "ready").exists():
          self.assertIsNone(server.poll(), "source exited before ready")
          self.assertLess(time.monotonic(), end, "source did not become ready")
          time.sleep(0.01)
        # A bridge that cannot yet register with the broker closes without
        # draining the initial reports. Unix seqpacket recv then raises ECONNRESET.
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as retry:
          retry.settimeout(1)
          retry.connect(str(directory / "event0"))
          self.assertEqual(len(retry.recv(4096, socket.MSG_PEEK)), 3 * 24)
        for index in (1, 0):
          with socket.socket(
            socket.AF_UNIX, socket.SOCK_SEQPACKET
          ) as controller:
            controller.settimeout(1)
            controller.connect(str(directory / f"event{index}"))
            self.assertEqual(len(controller.recv(4096)), 3 * 24)
            self.assertEqual(len(controller.recv(4096)), 2 * 24)
        # The same server still handles rumble after the input reconnections.
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as feedback:
          feedback.settimeout(1)
          feedback.connect(str(directory / "event0.ff"))
          effect = bytearray(48)
          struct.pack_into("<Hh", effect, 0, 0x50, -1)
          feedback.send(struct.pack("<IQ", 0x40304580, 0) + effect)
          reply = feedback.recv(4096)
          self.assertEqual(len(reply), 4 + 48)
          self.assertEqual(struct.unpack_from("<i", reply)[0], 0)
          self.assertEqual(struct.unpack_from("<h", reply, 6)[0], 0)
      finally:
        server.terminate()
        _, errors = server.communicate(timeout=2)
      self.assertNotIn("Traceback", errors)
