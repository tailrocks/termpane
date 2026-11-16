# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import contextlib
import hashlib
import inspect
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

import fixture_receipt
import verify_archive


def sha(data):
    return hashlib.sha256(data).hexdigest()


class ReceiptByteBindingTest(unittest.TestCase):
    def test_main_verifies_the_captured_bytes_when_path_is_replaced(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source"
            source.mkdir()
            (source / "src").mkdir()
            (source / "Cargo.toml").write_text('[package]\nname="termpane"\nversion="0.1.1"\nedition="2021"\n')
            (source / "src/lib.rs").write_text("pub mod grid;\n")
            (source / "src/grid.rs").write_text("pub enum EraseDisplayPolicy { Discard }\n")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=source, check=True, capture_output=True)
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            cargo_env = dict(os.environ, CARGO_TARGET_DIR=str(root / "cargo-output"))
            subprocess.run(["cargo", "package", "--offline", "--locked", "--no-verify"], cwd=source, env=cargo_env, check=True, capture_output=True)
            archive = root / "cargo-output/package/termpane-0.1.1.crate"
            captured = archive.read_bytes()
            replacement = root / "replacement.crate"
            replacement.write_bytes(b"not a crate")
            manifest = json.dumps({"version": "0.1.73", "commit": "c" * 40}).encode()
            common = ('repository = "tailrocks/termpane"\npackages = ["termpane"]\n'
                      '[recovery]\npackage = "termpane"\nversion = "0.1.1"\n'
                      f'source_sha = "{commit}"\n')
            supplied = (f'generator_version = "0.2.0"\ngenerator_source_sha = "{commit}"\n'
                        f'generator_manifest_sha256 = "{"0" * 64}"\n' + common).encode()
            effective = (f'generator_version = "0.1.73"\ngenerator_source_sha = "{"c" * 40}"\n'
                         f'generator_manifest_sha256 = "{sha(manifest)}"\n' + common).encode()
            evidence = {"schema": 1, "mode": "coordinate", "candidate_sha": commit,
                        "repository": "tailrocks/termpane", "source_sha": commit,
                        "package": "termpane", "version": "0.1.1", "tag_name": "termpane-v0.1.1",
                        "tag_target": commit, "supplied_intent_sha256": sha(supplied),
                        "effective_intent_sha256": sha(effective), "published_sha256": sha(captured),
                        "publication_started_ref": "d" * 40, "reconciled_ref": "e" * 40,
                        "uploads": 1, "oidc_requests": 1, "token_exchanges": 1, "token_revocations": 1}
            for name, data in (("intent", supplied), ("effective", effective), ("manifest", manifest),
                               ("evidence", json.dumps(evidence).encode())):
                (root / name).write_bytes(data)
            output = root / "bundle.tar"
            output.touch()
            source_lines, first_line = inspect.getsourcelines(fixture_receipt.main)
            boundary = next(first_line + i for i, line in enumerate(source_lines) if line.strip().startswith("verify("))
            original_trace = sys.gettrace()
            original_argv = sys.argv
            original_source = fixture_receipt.SOURCE_SHA
            original_verify_source = verify_archive.SOURCE_SHA
            previous_output = os.environ.get("VELNOR_RECEIPT_BUNDLE")
            replaced = False
            def trace(frame, event, arg):
                nonlocal replaced
                if frame.f_code is fixture_receipt.main.__code__ and event == "line" and frame.f_lineno == boundary:
                    os.replace(replacement, archive)
                    replaced = True
                return trace
            try:
                fixture_receipt.SOURCE_SHA = commit
                verify_archive.SOURCE_SHA = commit
                os.environ["VELNOR_RECEIPT_BUNDLE"] = str(output)
                sys.argv = ["fixture_receipt.py", str(root / "evidence"), str(archive), str(source),
                            str(source), str(root / "intent"), str(root / "effective"), str(root / "manifest"),
                            commit, "a" * 40, "511"]
                sys.settrace(trace)
                with contextlib.redirect_stdout(io.StringIO()):
                    fixture_receipt.main()
            finally:
                sys.settrace(original_trace)
                sys.argv = original_argv
                fixture_receipt.SOURCE_SHA = original_source
                verify_archive.SOURCE_SHA = original_verify_source
                if previous_output is None:
                    os.environ.pop("VELNOR_RECEIPT_BUNDLE", None)
                else:
                    os.environ["VELNOR_RECEIPT_BUNDLE"] = previous_output
            self.assertTrue(replaced)
            self.assertEqual(archive.read_bytes(), b"not a crate")
            with tarfile.open(output, "r:") as bundle:
                self.assertEqual(bundle.extractfile("termpane-0.1.1.crate").read(), captured)


if __name__ == "__main__":
    unittest.main()
