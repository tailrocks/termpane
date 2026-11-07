# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import concurrent.futures
import hashlib
import json
import pathlib
import re
import sys
import tomllib
import urllib.request
import urllib.parse

AGENT = "termpane-release-rehearsal/1.0 (https://github.com/tailrocks/termpane)"


def validate_registry_url(url):
    location = urllib.parse.urlparse(url)
    if location.scheme != "https" or location.hostname not in {"crates.io", "static.crates.io", "index.crates.io"}:
        raise ValueError("unexpected registry redirect")


class RegistryRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        validate_registry_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def read_url(url, limit):
    validate_registry_url(url)
    request = urllib.request.Request(url, headers={"User-Agent": AGENT})
    opener = urllib.request.build_opener(RegistryRedirect())
    with opener.open(request, timeout=30) as response:
        validate_registry_url(response.geturl())
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("registry response exceeds limit")
    return data


def index_path(name):
    name = name.lower()
    if len(name) == 1:
        return "1/" + name
    if len(name) == 2:
        return "2/" + name
    if len(name) == 3:
        return "3/" + name[0] + "/" + name
    return name[:2] + "/" + name[2:4] + "/" + name


def prepare(metadata_path, intent_path, destination):
    destination = pathlib.Path(destination)
    destination.mkdir(parents=True, exist_ok=False)
    snapshots = destination / "deps"
    snapshots.mkdir()
    metadata = json.loads(pathlib.Path(metadata_path).read_bytes())
    intent = tomllib.loads(pathlib.Path(intent_path).read_text())
    recovery = intent["recovery"]
    package = recovery["package"]
    version = recovery["version"]
    selected = [item for item in metadata["packages"] if item["name"] == package and item["version"] == version]
    if len(selected) != 1 or intent["packages"] != [package]:
        raise ValueError("intent does not select exactly one matching package")
    names = sorted({item["name"] for item in selected[0]["dependencies"] if item["name"] != package})
    if any(not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", name) for name in [package, *names]):
        raise ValueError("invalid crate name")

    def snapshot(name):
        data = read_url("https://crates.io/api/v1/crates/" + name, 8 * 1024 * 1024)
        body = json.loads(data)
        if body["crate"]["id"] != name or body["crate"]["name"] != name:
            raise ValueError("registry dependency identity mismatch")
        (snapshots / (name + ".json")).write_bytes(data)

    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        list(pool.map(snapshot, names))
    baseline = "0.1.0"
    index = read_url("https://index.crates.io/" + index_path(package), 8 * 1024 * 1024)
    matches = [json.loads(line) for line in index.splitlines() if line]
    matches = [item for item in matches if item.get("name") == package and item.get("vers") == baseline]
    if len(matches) != 1:
        raise ValueError("baseline index entry missing or ambiguous")
    archive = read_url(f"https://static.crates.io/crates/{package}/{package}-{baseline}.crate", 16 * 1024 * 1024)
    if hashlib.sha256(archive).hexdigest() != matches[0]["cksum"]:
        raise ValueError("baseline checksum differs from registry index")
    (destination / "index.jsonl").write_bytes(index)
    (destination / "baseline.crate").write_bytes(archive)
    print(f"Prepared {len(names)} dependency snapshots and checksum-verified {package} {baseline}")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: prepare_registry_inputs.py METADATA INTENT NEW_OUTPUT_DIRECTORY")
    prepare(*sys.argv[1:])
