#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0
import hashlib
import pathlib
import shutil
import sys
import tarfile
import tomllib


def index_path(name):
    name = name.lower()
    if len(name) == 1:
        return pathlib.Path("1") / name
    if len(name) == 2:
        return pathlib.Path("2") / name
    if len(name) == 3:
        return pathlib.Path("3") / name[0] / name
    return pathlib.Path(name[:2]) / name[2:4] / name


def main():
    cargo, source, baseline, destination = map(pathlib.Path, sys.argv[1:])
    index_root = "index.crates.io-1949cf8c6b5b557f"
    registry = cargo / "registry"
    destination.mkdir()
    target_index = destination / "index" / index_root
    target_cache = destination / "cache" / index_root
    target_cache.mkdir(parents=True)
    target_index.mkdir(parents=True)
    shutil.copyfile(registry / "index" / index_root / "config.json", target_index / "config.json")
    current = tomllib.loads((source / "Cargo.lock").read_text())
    with tarfile.open(baseline, "r:gz") as archive:
        previous = tomllib.loads(archive.extractfile("termpane-0.1.0/Cargo.lock").read().decode())
    for package in current["package"] + previous["package"]:
        if not package.get("source", "").startswith("registry+"):
            continue
        name, version = package["name"], package["version"]
        path = index_path(name)
        src_index = registry / "index" / index_root / ".cache" / path
        dst_index = target_index / ".cache" / path
        dst_index.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src_index, dst_index)
        filename = f"{name}-{version}.crate"
        src_crate = registry / "cache" / index_root / filename
        data = src_crate.read_bytes()
        if hashlib.sha256(data).hexdigest() != package["checksum"]:
            raise ValueError(f"checksum mismatch for {filename}")
        (target_cache / filename).write_bytes(data)


if __name__ == "__main__":
    main()
