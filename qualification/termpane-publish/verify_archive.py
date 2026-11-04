# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import json
import os
import pathlib
import posixpath
import subprocess
import sys
import tarfile
import tempfile

SOURCE_SHA = "f1c2dfde45bdbe91425af6bb17d80e606703baac"
PREFIX = "termpane-0.1.1/"


def git(source, *args):
    return subprocess.check_output(["git", "-C", str(source), *args])


def archive_files(path):
    result = {}
    with tarfile.open(path, "r:gz") as archive:
        for item in archive:
            if not item.isfile() or not item.name.startswith(PREFIX):
                raise ValueError("unexpected archive entry")
            name = item.name[len(PREFIX):]
            if not name or name.startswith("/") or posixpath.normpath(name) != name or name in result:
                raise ValueError("invalid or duplicate archive path")
            result[name] = archive.extractfile(item).read()
    return result


def verify(archive_path, source):
    source = pathlib.Path(source).resolve(strict=True)
    if git(source, "rev-parse", "HEAD").decode().strip() != SOURCE_SHA:
        raise ValueError("source SHA mismatch")
    if git(source, "status", "--porcelain"):
        raise ValueError("source checkout is not clean")
    listing = git(source, "ls-tree", "-rz", "--full-tree", SOURCE_SHA)
    tracked = {}
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        metadata, name = entry.split(b"\t", 1)
        mode, kind, object_id = metadata.decode().split(" ")
        if kind != "blob":
            raise ValueError("unexpected tracked object")
        tracked[name.decode()] = (mode, object_id)

    def source_bytes(name):
        seen = set()
        while True:
            if name in seen or name not in tracked:
                raise ValueError("untracked or cyclic archive entry: " + name)
            seen.add(name)
            mode, object_id = tracked[name]
            data = git(source, "cat-file", "blob", object_id)
            if mode != "120000":
                if mode not in ("100644", "100755"):
                    raise ValueError("unsupported source mode")
                return data
            target = data.decode()
            if target.startswith("/"):
                raise ValueError("absolute symlink")
            name = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
            if name == ".." or name.startswith("../"):
                raise ValueError("symlink escapes source")

    actual = archive_files(archive_path)
    vcs = json.loads(actual[".cargo_vcs_info.json"])
    if vcs.get("git", {}).get("sha1") != SOURCE_SHA or vcs.get("path_in_vcs", "") != "":
        raise ValueError("archive VCS identity mismatch")
    with tempfile.TemporaryDirectory(prefix="termpane-archive-check-") as target:
        env = dict(os.environ, CARGO_TARGET_DIR=target)
        subprocess.run(
            ["cargo", "package", "--offline", "--locked", "--no-verify"],
            cwd=source, env=env, check=True, stdout=subprocess.DEVNULL,
        )
        expected = archive_files(pathlib.Path(target) / "package" / (PREFIX[:-1] + ".crate"))
    if set(actual) != set(expected):
        raise ValueError("packaged file list differs from approved source")
    for name, data in actual.items():
        if name == ".cargo_vcs_info.json":
            if json.loads(data) != json.loads(expected[name]):
                raise ValueError("archive VCS metadata differs")
            continue
        if name == "Cargo.toml":
            if data != expected[name]:
                raise ValueError("normalized Cargo manifest differs")
            continue
        source_name = "Cargo.toml" if name == "Cargo.toml.orig" else name
        if data != source_bytes(source_name):
            raise ValueError("packaged file differs from approved source: " + name)
    if b"enum EraseDisplayPolicy" not in actual["src/grid.rs"]:
        raise ValueError("EraseDisplayPolicy missing")
    print(f"Verified {len(actual)} packaged files against {SOURCE_SHA}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: verify_archive.py ARCHIVE APPROVED_SOURCE")
    verify(sys.argv[1], sys.argv[2])
