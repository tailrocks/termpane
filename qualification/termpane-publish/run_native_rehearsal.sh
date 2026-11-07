#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
: "${CANDIDATE_SHA:?}" "${SOURCE_SHA:?}" "${CANDIDATE_PR:?}" "${VELNOR_REHEARSAL_OUTPUT:?}"
umask 077
unset GH_TOKEN GITHUB_TOKEN ACTIONS_ID_TOKEN_REQUEST_TOKEN ACTIONS_ID_TOKEN_REQUEST_URL ACTIONS_RUNTIME_TOKEN

control="$RUNNER_TEMP/control/qualification/termpane-publish"
assets="$RUNNER_TEMP/native-assets"
tools="$RUNNER_TEMP/native-tools"
runner="$RUNNER_TEMP/native-runner"
workspace="$RUNNER_TEMP/native-workspace"
rustup_home="$RUNNER_TEMP/native-rustup"
cargo_home="$RUNNER_TEMP/native-cargo"
target="$RUNNER_TEMP/native-target"
inputs="$RUNNER_TEMP/native-inputs"
image='rust@sha256:4c4c8ec55be09785e6d6ac45ea9853525a73c15db74009100ac08952e773dfb8'
source_sha='f1c2dfde45bdbe91425af6bb17d80e606703baac'
mkdir "$assets" "$tools" "$runner" "$workspace" "$rustup_home" "$cargo_home" "$target" "$RUNNER_TEMP/native-gh-config"
export GH_CONFIG_DIR="$RUNNER_TEMP/native-gh-config" GH_PROMPT_DISABLED=1
mkdir -p "$runner/coordinate/velnor/cargo" "$runner/coordinate/velnor/rustup" "$runner/coordinate/velnor-semver"

fetch() {
  local url="$1" path="$2" sha="$3"
  curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --max-time 180 --max-filesize 268435456 -o "$path" "$url"
  printf '%s  %s\n' "$sha" "$path" | sha256sum -c -
}
base='https://github.com/tailrocks/velnor-new/releases/download/v0.1.73'
fetch "$base/release-manifest.json" "$assets/release-manifest.json" '8e71b846acd375086fcb9693e567985d5a48b902e2e4b84400724ca6daad32d9'
fetch "$base/release-manifest.json.intoto.jsonl" "$assets/release-manifest.json.intoto.jsonl" '07acdc1fc01187e96d5545f7756cb769f3763ae2371f0a2785100545eac03f56'
fetch "$base/velnor-actions-0.1.73-x86_64-unknown-linux-gnu" "$assets/velnor-actions-0.1.73-x86_64-unknown-linux-gnu" 'e1b5224cedc462623de15056e75e76499245cb035bc70d20a2ca1a478bb68cd5'
fetch "$base/velnor-actions-0.1.73-x86_64-unknown-linux-gnu.intoto.jsonl" "$assets/velnor-actions-0.1.73-x86_64-unknown-linux-gnu.intoto.jsonl" '59292fc54dce584314f2664b6e9ac6d225f7f33c270c4688ed1f949a3f48ae4c'
fetch 'https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_linux_amd64.tar.gz' "$assets/gh.tar.gz" '4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d'
tar -xzf "$assets/gh.tar.gz" -C "$assets"
cp "$assets/gh_2.103.3_linux_amd64/bin/gh" "$tools/gh"
"$tools/gh" attestation trusted-root > "$assets/trusted-root.jsonl"
printf '%s  %s\n' '65ca537f6ed8a47fd0e560c421baa1f6c1efb8b25fc200d8c5c02c0e92eb2b9c' "$assets/trusted-root.jsonl" | sha256sum -c -
fetch 'https://github.com/jdx/mise/releases/download/v2026.10.25/mise-v2026.10.25-linux-x64' "$tools/mise" '5d362ecc578dc7c146679b841dda949d6472cbf86ff5635db7afdca1b6b3fd87'
chmod 500 "$tools/mise"
cp "$RUNNER_TEMP/nextest/cargo-nextest" "$tools/cargo-nextest"
cp "$RUNNER_TEMP/release-plz/release-plz" "$assets/release-plz"
fetch 'https://github.com/obi1kenobi/cargo-semver-checks/releases/download/v0.51.0/cargo-semver-checks-x86_64-unknown-linux-gnu.tar.gz' "$assets/semver.tar.gz" 'cacadafaa5cd27ae6d05a74a103266917ebab4c5e43c898360e177d4fbf06a65'
tar -xzf "$assets/semver.tar.gz" -C "$runner/coordinate/velnor-semver" cargo-semver-checks
printf '%s  %s\n' 'bc9eb06ce04e97d6160c875ffdee1f6e56793e423dea025f344826be03e66829' "$runner/coordinate/velnor-semver/cargo-semver-checks" | sha256sum -c -
fetch 'https://static.rust-lang.org/dist/channel-rust-1.97.1.toml' "$assets/channel.toml" '03569b1886ceb5c05276b50c8431ab111de944cd6140fe1fa7d821dd8e0f29cf'

test "$(git -C "$RUNNER_TEMP/source" rev-parse HEAD)" = "$source_sha"
test -z "$(git -C "$RUNNER_TEMP/source" status --porcelain)"
docker pull --platform linux/amd64 "$image"
docker run --platform linux/amd64 --rm -v "$rustup_home:/rustup" -e RUSTUP_HOME=/rustup "$image" rustup toolchain install 1.97.1 --profile minimal --no-self-update
docker run --platform linux/amd64 --rm \
  -v "$runner/coordinate:/runner/coordinate:ro" -v "$rustup_home:/runner/coordinate/velnor/rustup:ro" \
  -v "$RUNNER_TEMP/rust-verifier.sh:/rust-verifier.sh:ro" \
  -e RUNNER_TEMP=/verification -e RUSTUP_HOME=/runner/coordinate/velnor/rustup \
  -e CARGO_HOME=/runner/coordinate/velnor/cargo "$image" bash /rust-verifier.sh
docker run --platform linux/amd64 --rm --network none -v "$RUNNER_TEMP/source:/source:ro" -v "$RUNNER_TEMP/cargo-cache:/cargo" -e CARGO_HOME=/cargo -e RUSTUP_TOOLCHAIN=1.97.1 -w /source rust@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f cargo metadata --offline --locked --no-deps --format-version 1 > "$RUNNER_TEMP/native-metadata.json"
python3 "$control/prepare_registry_inputs.py" "$RUNNER_TEMP/native-metadata.json" "$RUNNER_TEMP/intent.toml" "$inputs"
cp "$RUNNER_TEMP/intent.toml" "$inputs/intent.toml"
bash "$control/assert_candidate_cargo_config.sh" "$RUNNER_TEMP/candidate"
docker run --platform linux/amd64 --rm \
  -v "$RUNNER_TEMP/candidate:/src:ro" -v "$cargo_home:/cargo" -w /src \
  -e CARGO_HOME=/cargo -e RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu -e RUSTUP_AUTO_INSTALL=0 \
  -e GIT_CONFIG_NOSYSTEM=1 -e GIT_CONFIG_GLOBAL=/dev/null -e GIT_TERMINAL_PROMPT=0 \
  "$image" cargo fetch --locked
# Only Cargo's downloaded registry and Git caches belong in this home.
# Project configuration is reviewed in the candidate checkout; a writable
# Cargo-home config or credential file must not survive into the native run.
find "$cargo_home" -mindepth 1 -maxdepth 1 ! -name registry ! -name git -exec rm -rf -- {} +
unexpected_cache_entry="$(find "$cargo_home" ! -type f ! -type d -print -quit)"
test -z "$unexpected_cache_entry"
mkdir -p "$cargo_home/registry"
cp -an "$RUNNER_TEMP/cargo-cache/registry/." "$cargo_home/registry/"
python3 "$control/prepare_offline_registry.py" "$cargo_home" "$RUNNER_TEMP/source" "$inputs/baseline.crate" "$RUNNER_TEMP/native-registry"

# Run the candidate's production Git acquisition before the offline coordinator
# starts and before the approved source or its build scripts can execute.
docker run --platform linux/amd64 --rm \
  -v "$RUNNER_TEMP/candidate:/src:ro" -v "$cargo_home:/cargo" -v "$target:/target" -v "$tools:/tools:ro" \
  -v "$runner/coordinate:/runner/coordinate" -w /src \
  -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target -e RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu -e RUSTUP_AUTO_INSTALL=0 \
  -e VELNOR_NATIVE_ACQUIRE_GIT=/runner/coordinate \
  -e PATH=/tools:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  "$image" cargo nextest run --offline --locked -p velnor-actions-orchestrator --lib \
  -E 'test(generated_candidate_receipt_rejects_unprotected_environment)' --no-tests fail

docker run --platform linux/amd64 --rm --network none \
  --add-host fixture.actions.githubusercontent.com:127.0.0.1 \
  --add-host crates.io:127.0.0.1 --add-host api.github.com:127.0.0.1 --add-host github.com:127.0.0.1 \
  -v "$RUNNER_TEMP/candidate:/src:ro" -v "$cargo_home:/cargo" -v "$target:/target" -v "$tools:/tools:ro" \
  -v "$RUNNER_TEMP/candidate/crates/velnor-actions-orchestrator/tests/fixtures/native_candidate:/fixture:ro" \
  -v "$assets:/assets:ro" -v "$workspace:/workspace" -v "$runner:/runner" \
  -v "$rustup_home:/runner/coordinate/velnor/rustup" -v "$RUNNER_TEMP/source:/input-source:ro" -v "$inputs:/inputs:ro" \
  -v "$RUNNER_TEMP/native-registry:/fixture-registry:ro" \
  -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target -e RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu -e RUSTUP_AUTO_INSTALL=0 \
  -e VELNOR_FIXTURE_SUPPLIED_INTENT=/inputs/intent.toml -e VELNOR_FIXTURE_SUPPLIED_SOURCE=/input-source \
  -e VELNOR_FIXTURE_BASELINE_VERSION=0.1.0 -e VELNOR_FIXTURE_BASELINE_ARCHIVE=/inputs/baseline.crate \
  -e VELNOR_FIXTURE_BASELINE_INDEX=/inputs/index.jsonl -e VELNOR_FIXTURE_DEPENDENCY_SNAPSHOTS=/inputs/deps \
  -e VELNOR_NATIVE_REGISTRY_SNAPSHOT=/fixture-registry \
  -e VELNOR_NATIVE_FIXTURE_EVIDENCE=/runner/coordinate/native-evidence.json \
  -e PATH=/tools:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  "$image" python3 /fixture/drive.py coordinate

for input in "$runner/coordinate/fixture-package/package/termpane-0.1.1.crate" \
  "$RUNNER_TEMP/intent.toml" "$workspace/final-coordinate/.github/velnor-release-intent.toml" \
  "$runner/coordinate/native-evidence.json" "$assets/release-manifest.json"; do
  test ! -L "$input" && test -f "$input"
done

python3 - "$VELNOR_REHEARSAL_OUTPUT" <<'PYTHON'
import os
import sys
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
os.close(fd)
PYTHON

docker run --platform linux/amd64 --rm --network none \
  -v "$RUNNER_TEMP/source:/source:ro" -v "$RUNNER_TEMP/cargo-cache:/cargo" \
  -v "$runner/coordinate/fixture-package/package/termpane-0.1.1.crate:/archive.crate:ro" \
  -v "$control/verify_archive.py:/verify_archive.py:ro" \
  -v "$control/fixture_receipt.py:/fixture_receipt.py:ro" \
  -v "$RUNNER_TEMP/candidate:/candidate:ro" \
  -v "$RUNNER_TEMP/intent.toml:/intent.toml:ro" \
  -v "$workspace/final-coordinate/.github/velnor-release-intent.toml:/effective.toml:ro" \
  -v "$runner/coordinate/native-evidence.json:/native-evidence.json:ro" \
  -v "$assets/release-manifest.json:/manifest.json:ro" \
  -v "$VELNOR_REHEARSAL_OUTPUT:/bundle.tar" \
  -e VELNOR_RECEIPT_BUNDLE=/bundle.tar \
  -e CARGO_HOME=/cargo -e RUSTUP_TOOLCHAIN=1.97.1 -w /source \
  rust@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f \
  python3 /fixture_receipt.py /native-evidence.json /archive.crate /source /candidate /intent.toml /effective.toml /manifest.json "$CANDIDATE_SHA" "$SOURCE_SHA" "$CANDIDATE_PR"
