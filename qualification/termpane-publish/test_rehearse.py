# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("rehearse.sh")


class RehearsalWrapperTest(unittest.TestCase):
    def test_wrong_or_dirty_control_checkout_is_rejected_before_use(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "reviewed").write_text("reviewed\n")
            subprocess.run(["git", "-C", str(source), "add", "reviewed"], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=Test", "-c",
                            "user.email=test@example.invalid", "commit", "-qm", "fixture"], check=True)
            head = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            env = dict(os.environ, RUNNER_TEMP=temp, SOURCE_SHA="a" * 40,
                       CANDIDATE_PR="1", CANDIDATE_SHA="b" * 40,
                       VELNOR_REHEARSAL_OUTPUT=str(root / "rehearsal-output.tar"))
            result = subprocess.run(["bash", str(SCRIPT)], cwd=source, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(source.is_dir())
            self.assertFalse((root / "control").exists())

            (source / "untracked").write_text("unexpected\n")
            env["SOURCE_SHA"] = head
            result = subprocess.run(["bash", str(SCRIPT)], cwd=source, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(source.is_dir())
            self.assertFalse((root / "control").exists())

    def test_invalid_candidate_is_rejected_before_moving_reviewed_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source"
            source.mkdir()
            env = dict(os.environ, RUNNER_TEMP=temp, SOURCE_SHA="a" * 40,
                       CANDIDATE_PR="1;bad", CANDIDATE_SHA="b" * 40,
                       VELNOR_REHEARSAL_OUTPUT=str(root / "rehearsal-output.tar"))
            result = subprocess.run(["bash", str(SCRIPT)], cwd=source, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(source.is_dir())
            self.assertFalse((root / "control").exists())



if __name__ == "__main__":
    unittest.main()
