"""The dev runner's Flatpak command line (tools/dev/run.py)."""

import importlib.util
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[3]
PROFILE = "unittest-dev-runner"


def load_runner(env):
  with mock.patch.dict(os.environ, env):
    spec = importlib.util.spec_from_file_location(
      "dev_runner", REPO / "tools/dev/run.py"
    )
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
  return runner


class DevRunnerTests(unittest.TestCase):
  def setUp(self):
    scratch = REPO / "out/test"
    scratch.mkdir(parents=True, exist_ok=True)
    self.work = Path(tempfile.mkdtemp(dir=scratch))
    self.addCleanup(shutil.rmtree, self.work)
    self.addCleanup(
      shutil.rmtree, REPO / "out/dev/flatpak" / PROFILE, ignore_errors=True
    )

  def test_every_shared_directory_exists_before_flatpak_run(self):
    images = self.work / "images"
    data = self.work / "profile/android-data"
    runner = load_runner(
      {
        "INNKEEPER_PROFILE": PROFILE,
        "INNKEEPER_IMAGES": str(images),
        "INNKEEPER_DATA": str(data),
      }
    )
    argv = runner.command("session", ["start", "-f"])
    shared = [
      Path(a.split("=", 1)[1])
      for a in argv
      if a.startswith("--filesystem=") and not a.endswith(":ro")
    ]
    self.assertIn(images, shared)
    self.assertIn(data.parent, shared)
    for path in shared:
      self.assertTrue(path.is_dir(), f"{path} would not be mounted")

  def test_stale_sessions_keeps_units_past_the_limit(self):
    runner = load_runner({"INNKEEPER_PROFILE": PROFILE})
    now = 10 * runner.STALE_US
    old = (
      "app-flatpak-dev.sargunv.Innkeeper-1.scope",
      now - runner.STALE_US - 1,
      "?",
    )
    young = (
      "app-flatpak-dev.sargunv.Innkeeper-2.scope",
      now - runner.STALE_US,
      "?",
    )
    self.assertEqual(
      runner.stale_sessions([old, young], now, runner.STALE_US), [old]
    )

  def test_format_stale_lists_each_session(self):
    runner = load_runner({"INNKEEPER_PROFILE": PROFILE})
    unit = "app-flatpak-dev.sargunv.Innkeeper-123.scope"
    now = 23 * 60 * 60 * 1_000_000
    text = runner.format_stale([(unit, 0, "INNKEEPER_PROFILE=r310")], now)
    self.assertEqual(
      text,
      "run.py: these Innkeeper dev sessions have run for more than 2 hours:\n"
      f"  {unit}  23 h  INNKEEPER_PROFILE=r310\n"
      "Stop one with: systemctl --user stop <unit>\n",
    )


if __name__ == "__main__":
  unittest.main()
