"""Private desktop cleanup preserves the command result and reports leaks."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).parents[1] / "nested.sh"
OUTPUT = SOURCE.parents[1] / "out/test"


class NestedCleanupTest(unittest.TestCase):
  def test_portal_unmount_and_cleanup_failures_preserve_command_status(self):
    source = SOURCE.read_text()
    cleanup = source[
      source.index("cleanup() {") : source.index("\ntrap cleanup EXIT")
    ]
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for mode in ("normal", "busy", "mounted", "remove-error"):
      for status in (0, 17):
        with (
          self.subTest(mode=mode, status=status),
          tempfile.TemporaryDirectory(dir=OUTPUT) as work,
        ):
          work = Path(work)
          runtime = work / "lapk-private"
          runtime.mkdir()
          (runtime / "mounted").touch()
          binary = work / "bin"
          binary.mkdir()
          dispatcher = binary / "dispatch"
          dispatcher.write_text("""#!/usr/bin/python3
import json, os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
rt = pathlib.Path(os.environ["rt"])
mode = os.environ["mode"]
with open(os.environ["trace"], "a") as log:
  log.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
if name == "gdbus":
  print("(uint32 424242,)")
elif name == "mountpoint":
  sys.exit(0 if (rt / "mounted").exists() else 1)
elif name == "fusermount3":
  assert sys.argv[-1] == str(rt / "doc")
  if mode == "mounted" or (mode == "busy" and "-uz" not in sys.argv):
    sys.exit(1)
  (rt / "mounted").unlink(missing_ok=True)
elif name == "rm":
  assert sys.argv[-1] == str(rt)
  if mode == "remove-error":
    sys.exit(1)
  shutil.rmtree(rt)
""")
          dispatcher.chmod(0o755)
          for command in (
            "journalctl",
            "systemctl",
            "gdbus",
            "mountpoint",
            "fusermount3",
            "rm",
          ):
            (binary / command).symlink_to(dispatcher)
          env = dict(
            os.environ,
            PATH=f"{binary}:{os.environ['PATH']}",
            rt=str(runtime),
            host_runtime=str(work),
            N=str(work),
            repo=str(work),
            unit="nested-test",
            phase="command",
            LINUXAPK_TEST_COMPOSITOR="test",
            LINUXAPK_E2E="0",
            private_bus=f"unix:path={runtime}/bus",
            mode=mode,
            trace=str(work / "trace"),
          )
          script = (
            """set -Eeuo pipefail
kill() { printf '["kill", "%s"]\\n' "$*" >>"$trace"; }
"""
            + cleanup
          )
          script += f"\ntrap cleanup EXIT\nexit {status}\n"
          result = subprocess.run(
            ["bash", "-c", script],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
          )
          self.assertEqual(status, result.returncode, result.stderr)
          calls = [
            json.loads(line)
            for line in (work / "trace").read_text().splitlines()
          ]
          names = [call[0] for call in calls]
          self.assertIn(["kill", "424242"], calls)
          query = calls[names.index("gdbus")]
          self.assertIn(env["private_bus"], query)
          self.assertLess(names.index("gdbus"), names.index("kill"))
          self.assertLess(names.index("kill"), names.index("systemctl"))
          self.assertLess(names.index("systemctl"), names.index("fusermount3"))
          unmounts = [call for call in calls if call[0] == "fusermount3"]
          self.assertEqual(
            2 if mode in ("busy", "mounted") else 1, len(unmounts)
          )
          if mode == "mounted":
            self.assertNotIn("rm", names)
          if mode in ("mounted", "remove-error"):
            self.assertIn("leaked private runtime", result.stderr)
            self.assertTrue(runtime.exists())
          else:
            self.assertFalse(runtime.exists())
