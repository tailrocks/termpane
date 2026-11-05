# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tarfile
import tomllib

from verify_archive import SOURCE_SHA, verify

MAX_EVIDENCE = 16 * 1024
MAX_INTENT = 1024 * 1024
MAX_PACKAGE = 100 * 1024 * 1024
HEX40 = re.compile(r"[0-9a-f]{40}\Z")


def bounded(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise ValueError("input is not a bounded regular file")
        data = source.read(limit + 1)
    if not data or len(data) > limit:
        raise ValueError("input size invalid")
    return data


def git_identity(path, expected):
    path = Path(path).resolve(strict=True)
    def run(*args):
        return subprocess.check_output(
            ["git", "-c", f"safe.directory={path}", "-C", str(path), *args], text=True
        ).strip()
    commit = run("rev-parse", "HEAD^{commit}")
    tree = run("rev-parse", "HEAD^{tree}")
    if not HEX40.fullmatch(commit) or not HEX40.fullmatch(tree) or commit != expected or run("status", "--porcelain"):
        raise ValueError("Git checkout does not match the expected clean commit")
    return {"commit": commit, "tree": tree}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate evidence field")
        result[key] = value
    return result


def validate_evidence(evidence, candidate, supplied, effective, packed, manifest):
    fields = {"schema", "mode", "candidate_sha", "repository", "source_sha", "package", "version",
              "supplied_intent_sha256", "effective_intent_sha256", "published_sha256", "tag_name",
              "tag_target", "publication_started_ref", "reconciled_ref", "uploads", "oidc_requests",
              "token_exchanges", "token_revocations"}
    if set(evidence) != fields:
        raise ValueError("unexpected fixture evidence fields")
    intent = tomllib.loads(supplied.decode("utf-8"))
    actual = tomllib.loads(effective.decode("utf-8"))
    expected_generator = json.loads(manifest)
    for data in (intent, actual):
        if (data.get("repository") != "tailrocks/termpane" or data.get("packages") != ["termpane"]
                or data.get("recovery") != {"package": "termpane", "version": "0.1.1", "source_sha": SOURCE_SHA}):
            raise ValueError("intent differs from approved release")
    if intent.get("generator_source_sha") != candidate:
        raise ValueError("intent does not bind the candidate")
    if (actual.get("generator_version") != expected_generator.get("version")
            or actual.get("generator_source_sha") != expected_generator.get("commit")
            or actual.get("generator_manifest_sha256") != hashlib.sha256(manifest).hexdigest()):
        raise ValueError("effective fixture generator differs from manifest")
    for key in ("generator_version", "generator_source_sha", "generator_manifest_sha256"):
        intent.pop(key, None)
        actual.pop(key, None)
    if intent != actual:
        raise ValueError("effective fixture intent differs from supplied intent")
    expected = {"schema": 1, "mode": "coordinate", "candidate_sha": candidate,
                "repository": "tailrocks/termpane", "source_sha": SOURCE_SHA,
                "package": "termpane", "version": "0.1.1",
                "supplied_intent_sha256": hashlib.sha256(supplied).hexdigest(),
                "effective_intent_sha256": hashlib.sha256(effective).hexdigest(),
                "published_sha256": hashlib.sha256(packed).hexdigest(),
                "tag_name": "termpane-v0.1.1", "tag_target": SOURCE_SHA,
                "uploads": 1, "oidc_requests": 1, "token_exchanges": 1, "token_revocations": 1}
    if any(type(evidence.get(key)) is not type(value) or evidence[key] != value for key, value in expected.items()):
        raise ValueError("fixture observation differs from verified inputs")
    if not all(isinstance(evidence[key], str) and HEX40.fullmatch(evidence[key])
               for key in ("publication_started_ref", "reconciled_ref")):
        raise ValueError("fixture journal object identity invalid")
    if evidence["publication_started_ref"] == evidence["reconciled_ref"]:
        raise ValueError("fixture journal object identities are not distinct")
    return evidence



def write_bundle(path, files):
    expected = {"receipt.json", "fixture-evidence.json", "supplied-intent.toml",
                "effective-intent.toml", "generator-manifest.json", "termpane-0.1.1.crate"}
    if set(files) != expected or any(not isinstance(value, bytes) or not value for value in files.values()):
        raise ValueError("invalid bundle inputs")
    size = sum(512 + ((len(value) + 511) // 512) * 512 for value in files.values()) + 10240
    if size > MAX_PACKAGE:
        raise ValueError("bundle exceeds limit")
    descriptor = os.open(path, os.O_WRONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "wb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size != 0:
            raise ValueError("bundle destination must be an empty regular file")
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, value in sorted(files.items()):
                entry = tarfile.TarInfo(name)
                entry.size = len(value)
                entry.mode = 0o600
                archive.addfile(entry, io.BytesIO(value))
        stream.flush()
        os.fsync(stream.fileno())


def main():
    if len(sys.argv) != 11:
        raise SystemExit("usage: fixture_receipt.py EVIDENCE ARCHIVE SOURCE CANDIDATE INTENT EFFECTIVE MANIFEST CANDIDATE_SHA CONTROL_SHA PR")
    evidence_path, archive, source, candidate, intent, effective, manifest, candidate_sha, control_sha, pull = sys.argv[1:]
    if not HEX40.fullmatch(candidate_sha) or not HEX40.fullmatch(control_sha) or not re.fullmatch(r"[1-9][0-9]{0,11}", pull):
        raise ValueError("invalid commit identity")
    candidate_identity = git_identity(candidate, candidate_sha)
    source_identity = git_identity(source, SOURCE_SHA)
    raw = bounded(evidence_path, MAX_EVIDENCE)
    evidence = json.loads(raw, object_pairs_hook=unique_object)
    supplied_bytes = bounded(intent, MAX_INTENT)
    effective_bytes = bounded(effective, MAX_INTENT)
    package_bytes = bounded(archive, MAX_PACKAGE)
    manifest_bytes = bounded(manifest, MAX_INTENT)
    verified = validate_evidence(evidence, candidate_sha, supplied_bytes, effective_bytes,
                                 package_bytes, manifest_bytes)
    verify(io.BytesIO(package_bytes), source)
    receipt = {"schema": 1, "kind": "untrusted-offline-fixture-observation",
               "control_commit": control_sha, "candidate_pr": int(pull), "candidate": candidate_identity,
               "source": source_identity, "fixture": verified}
    encoded = json.dumps(receipt, sort_keys=True, separators=(",", ":"))
    if len(encoded.encode()) > MAX_EVIDENCE:
        raise ValueError("receipt exceeds limit")
    output = os.environ.get("VELNOR_RECEIPT_BUNDLE")
    if not output:
        raise ValueError("bundle destination is required")
    write_bundle(output, {"receipt.json": (encoded + "\n").encode(), "fixture-evidence.json": raw,
                          "supplied-intent.toml": supplied_bytes, "effective-intent.toml": effective_bytes,
                          "generator-manifest.json": manifest_bytes, "termpane-0.1.1.crate": package_bytes})
    print("TERMPANE_FIXTURE_OBSERVATION=" + encoded)


if __name__ == "__main__":
    main()
