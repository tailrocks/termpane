# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("rehearse.sh")


class RehearsalWrapperTest(unittest.TestCase):
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
