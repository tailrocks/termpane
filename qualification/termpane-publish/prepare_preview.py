# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0

import json
import pathlib
import re
import shutil
import sys
import tomllib


def main():
    preview, candidate, config, sha = sys.argv[1:]
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("invalid candidate SHA")
    preview = pathlib.Path(preview).resolve(strict=True)
    candidate = pathlib.Path(candidate).resolve(strict=True)
    config = pathlib.Path(config).resolve(strict=True)
    version = tomllib.loads((candidate / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    repository = "tailrocks/velnor-new"
    targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"]
    manifest = {
        "schema": 1, "version": version, "repository": repository, "commit": sha,
        "targets": [{"target": target,
                     "artifact": f"https://github.com/{repository}/releases/download/v{version}/velnor-actions-{version}-{target}",
                     "sha256": "0" * 64} for target in targets],
    }
    shutil.copyfile(config, preview / ".velnor/config.toml")
    (preview / ".velnor/release-manifest.json").write_text(json.dumps(manifest))


if __name__ == "__main__":
    main()
