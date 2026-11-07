# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import hashlib
import json
import pathlib
import tempfile
import unittest
from unittest.mock import patch

import prepare_registry_inputs as inputs


class PrepareRegistryInputsTests(unittest.TestCase):
    def prepare(self, checksum):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            metadata = root / "metadata.json"
            metadata.write_text(json.dumps({"packages": [{"name": "demo", "version": "0.1.1", "dependencies": [{"name": "dep"}]}]}))
            intent = root / "intent.toml"
            intent.write_text('packages = ["demo"]\n[recovery]\npackage = "demo"\nversion = "0.1.1"\n')
            archive = b"fixture archive"
            index = json.dumps({"name": "demo", "vers": "0.1.0", "cksum": checksum}).encode() + b"\n"
            dependency = json.dumps({"crate": {"id": "dep", "name": "dep"}}).encode()

            def response(url, _limit):
                if url.endswith("/api/v1/crates/dep"):
                    return dependency
                if url.endswith("/de/mo/demo"):
                    return index
                if url.endswith("/demo-0.1.0.crate"):
                    return archive
                raise AssertionError(url)

            with patch.object(inputs, "read_url", response):
                inputs.prepare(metadata, intent, root / "out")
            return (root / "out" / "baseline.crate").read_bytes()

    def test_fetches_dependency_and_checksum_verified_baseline(self):
        archive = b"fixture archive"
        self.assertEqual(self.prepare(hashlib.sha256(archive).hexdigest()), archive)

    def test_rejects_wrong_baseline_checksum(self):
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.prepare("0" * 64)

    def test_rejects_untrusted_or_insecure_redirects_before_following(self):
        handler = inputs.RegistryRedirect()
        for url in ("http://crates.io/x", "https://other.invalid/x"):
            with self.assertRaisesRegex(ValueError, "redirect"):
                handler.redirect_request(None, None, 302, "Found", {}, url)

    def test_rejects_oversized_registry_response(self):
        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return None

            def geturl(self):
                return "https://crates.io/test"

            def read(self, _length):
                return b"12345"

        class Opener:
            def open(self, _request, timeout):
                self.timeout = timeout
                return Response()

        with patch.object(inputs.urllib.request, "build_opener", return_value=Opener()):
            with self.assertRaisesRegex(ValueError, "exceeds limit"):
                inputs.read_url("https://crates.io/test", 4)

    def test_rejects_invalid_dependency_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            metadata = root / "metadata.json"
            metadata.write_text(json.dumps({"packages": [{"name": "demo", "version": "0.1.1", "dependencies": [{"name": "../bad"}]}]}))
            intent = root / "intent.toml"
            intent.write_text('packages = ["demo"]\n[recovery]\npackage = "demo"\nversion = "0.1.1"\n')
            with self.assertRaisesRegex(ValueError, "invalid crate name"):
                inputs.prepare(metadata, intent, root / "out")

    def test_rejects_dependency_identity_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            metadata = root / "metadata.json"
            metadata.write_text(json.dumps({"packages": [{"name": "demo", "version": "0.1.1", "dependencies": [{"name": "dep"}]}]}))
            intent = root / "intent.toml"
            intent.write_text('packages = ["demo"]\n[recovery]\npackage = "demo"\nversion = "0.1.1"\n')
            wrong = json.dumps({"crate": {"id": "other", "name": "other"}}).encode()
            with patch.object(inputs, "read_url", return_value=wrong):
                with self.assertRaisesRegex(ValueError, "identity mismatch"):
                    inputs.prepare(metadata, intent, root / "out")

    def test_index_path_uses_cargo_name_layout(self):
        self.assertEqual(inputs.index_path("a"), "1/a")
        self.assertEqual(inputs.index_path("AB"), "2/ab")
        self.assertEqual(inputs.index_path("AbC"), "3/a/abc")
        self.assertEqual(inputs.index_path("Demo"), "de/mo/demo")


if __name__ == "__main__":
    unittest.main()
