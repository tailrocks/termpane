# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import tarfile
import unittest

from fixture_receipt import SOURCE_SHA, bounded, git_identity, unique_object, validate_evidence
from verify_archive import MAX_FILE, archive_files

CANDIDATE = "b" * 40


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fixture():
    manifest = json.dumps({"version": "0.1.73", "commit": "c" * 40}).encode()
    common = ('repository = "tailrocks/termpane"\npackages = ["termpane"]\n'
              '[recovery]\npackage = "termpane"\nversion = "0.1.1"\n'
              f'source_sha = "{SOURCE_SHA}"\n')
    supplied = (f'generator_version = "0.2.0"\ngenerator_source_sha = "{CANDIDATE}"\n'
                f'generator_manifest_sha256 = "{"0" * 64}"\n' + common).encode()
    effective = (f'generator_version = "0.1.73"\ngenerator_source_sha = "{"c" * 40}"\n'
                 f'generator_manifest_sha256 = "{sha(manifest)}"\n' + common).encode()
    packed = b"actual fixture package"
    evidence = {"schema": 1, "mode": "coordinate", "candidate_sha": CANDIDATE,
                "repository": "tailrocks/termpane", "source_sha": SOURCE_SHA,
                "package": "termpane", "version": "0.1.1", "tag_name": "termpane-v0.1.1",
                "tag_target": SOURCE_SHA, "supplied_intent_sha256": sha(supplied),
                "effective_intent_sha256": sha(effective), "published_sha256": sha(packed),
                "publication_started_ref": "d" * 40, "reconciled_ref": "e" * 40,
                "uploads": 1, "oidc_requests": 1, "token_exchanges": 1, "token_revocations": 1}
    return evidence, supplied, effective, packed, manifest


class FixtureReceiptTest(unittest.TestCase):
    def test_actual_input_bytes_and_each_observed_binding_are_required(self):
        evidence, supplied, effective, packed, manifest = fixture()
        self.assertEqual(validate_evidence(evidence, CANDIDATE, supplied, effective, packed, manifest), evidence)
        for field in evidence:
            with self.subTest(field=field):
                changed = copy.deepcopy(evidence)
                changed[field] = False if type(changed[field]) is int else "changed"
                with self.assertRaises(ValueError):
                    validate_evidence(changed, CANDIDATE, supplied, effective, packed, manifest)
        for position in range(4):
            inputs = [supplied, effective, packed, manifest]
            inputs[position] += b"changed"
            with self.subTest(input=position), self.assertRaises((ValueError, UnicodeDecodeError)):
                validate_evidence(evidence, CANDIDATE, *inputs)
        with self.assertRaises(ValueError):
            validate_evidence({**evidence, "extra": "forged"}, CANDIDATE, supplied, effective, packed, manifest)

    def test_archive_rejects_parent_traversal_entries(self):
        for name in ("../escape", "..", "../../outside"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                archive = Path(temp) / "traversal.crate"
                with tarfile.open(archive, "w:gz") as output:
                    entry = tarfile.TarInfo("termpane-0.1.1/" + name)
                    entry.size = 1
                    output.addfile(entry, io.BytesIO(b"x"))
                with self.assertRaises(ValueError):
                    archive_files(archive)

    def test_archive_rejects_oversized_pax_metadata(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "oversized-pax.crate"
            with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT) as output:
                entry = tarfile.TarInfo("termpane-0.1.1/a")
                entry.pax_headers = {"comment": "x" * (17 * 1024 * 1024)}
                entry.size = 1
                output.addfile(entry, io.BytesIO(b"x"))
            with self.assertRaises(ValueError):
                archive_files(path)

    def test_archive_rejects_oversized_entry_before_extraction(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "oversized.crate"
            entry = tarfile.TarInfo("termpane-0.1.1/oversized")
            entry.size = MAX_FILE + 1
            with gzip.open(path, "wb") as output:
                output.write(entry.tobuf())
            with self.assertRaises(ValueError):
                archive_files(path)

    def test_real_git_checkout_must_match_commit_and_be_clean(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "file").write_text("content")
            subprocess.run(["git", "-C", temp, "add", "file"], check=True)
            subprocess.run(["git", "-C", temp, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", temp, "rev-parse", "HEAD"], text=True).strip()
            self.assertEqual(git_identity(root, commit)["commit"], commit)
            (root / "file").write_text("changed")
            with self.assertRaises(ValueError):
                git_identity(root, commit)
            with self.assertRaises(ValueError):
                git_identity(root, "a" * 40)

    def test_input_bounds_and_duplicate_json_fields_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "data"
            path.write_bytes(b"12345")
            with self.assertRaises(ValueError):
                bounded(path, 4)
            link = Path(temp) / "link"
            link.symlink_to(path)
            with self.assertRaises(OSError):
                bounded(link, 10)
            with self.assertRaises(ValueError):
                json.loads('{"a":1,"a":2}', object_pairs_hook=unique_object)


if __name__ == "__main__":
    unittest.main()
