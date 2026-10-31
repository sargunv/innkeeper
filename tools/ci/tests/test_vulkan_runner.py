"""The default Vulkan runner must stay on the launcher's selected GPU."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


class VulkanRunnerTests(unittest.TestCase):
  def test_only_selected_hardware_is_tested(self):
    scratch = REPO / "out/test"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
      repo = Path(directory)
      runner = repo / "android/vendor/vulkan/tests/run.sh"
      runner.parent.mkdir(parents=True)
      shutil.copy2(REPO / "android/vendor/vulkan/tests/run.sh", runner)
      binary = repo / "out/build/vulkan/test/vkdriver_test"
      binary.parent.mkdir(parents=True)
      binary.touch(mode=0o755)
      bionic = repo / "tests/bionic/run.sh"
      bionic.parent.mkdir(parents=True)
      bionic.write_text(
        "#!/bin/bash\n"
        "if [[ $* == *getprop* ]]; then\n"
        '  echo "innkeeper-selected-driver=$TEST_SELECTED_DRIVER"\n'
        "fi\n"
      )
      bionic.chmod(0o755)
      for selected in ["radv", "anv", "lvp"]:
        with self.subTest(selected=selected):
          output = subprocess.check_output(
            [str(runner)],
            env={**os.environ, "TEST_SELECTED_DRIVER": selected},
            text=True,
          )
          expected = ["lvp"] + ([] if selected == "lvp" else [selected])
          self.assertEqual(
            output.splitlines(),
            [f"== vulkan tests: {driver}" for driver in expected],
          )
