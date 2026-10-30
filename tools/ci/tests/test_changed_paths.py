"""The docs fast path must never skip unknown or removed code inputs."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "changed-paths.sh"


class ChangedPaths(unittest.TestCase):
  def test_classification(self):
    with tempfile.TemporaryDirectory() as directory:
      repo = Path(directory)

      def git(*args):
        return subprocess.check_output(
          ["git", *args], cwd=repo, text=True
        ).strip()

      git("init", "-q")
      git("config", "user.name", "CI test")
      git("config", "user.email", "ci@example.invalid")

      def commit(path, text):
        file = repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)
        git("add", ".")
        git("commit", "-qm", "fixture")
        return git("rev-parse", "HEAD")

      base = commit("README.md", "baseline")

      def code(head, start=base):
        env = dict(os.environ)
        env.pop("GITHUB_OUTPUT", None)
        return subprocess.check_output(
          [str(SCRIPT), start, head], cwd=repo, env=env, text=True
        ).strip()

      docs = commit("docs/read me.md", "documentation")
      self.assertEqual(code(docs), "code=false")
      unknown = commit("android/overlay/system/etc/fixture.md", "")
      self.assertEqual(code(unknown), "code=true")
      git("rm", "android/overlay/system/etc/fixture.md")
      git("commit", "-qm", "delete code input")
      self.assertEqual(code(git("rev-parse", "HEAD"), unknown), "code=true")
      self.assertEqual(code(docs, "0" * 40), "code=true")
      self.assertEqual(code(docs, "missing-history"), "code=true")
