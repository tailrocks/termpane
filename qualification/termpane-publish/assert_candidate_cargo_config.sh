#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
candidate="${1:?}"
test ! -L "$candidate/.cargo"
for config in "$candidate/.cargo/config" "$candidate/.cargo/config.toml"; do
  test ! -e "$config"
  test ! -L "$config"
done
