#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Alexey Zhokhov
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
umask 077
: "${RUNNER_TEMP:?}" "${SOURCE_SHA:?}" "${CANDIDATE_PR:?}" "${CANDIDATE_SHA:?}" "${VELNOR_REHEARSAL_OUTPUT:?}"
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ && "$CANDIDATE_SHA" =~ ^[0-9a-f]{40}$ && "$CANDIDATE_PR" =~ ^[1-9][0-9]*$ ]]
[[ "$(pwd -P)" = "$RUNNER_TEMP/source" ]]
[[ "$VELNOR_REHEARSAL_OUTPUT" = "$RUNNER_TEMP/rehearsal-output.tar" ]]
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 RUSTUP_AUTO_INSTALL=0
unset GH_TOKEN GITHUB_TOKEN ACTIONS_ID_TOKEN_REQUEST_TOKEN ACTIONS_ID_TOKEN_REQUEST_URL ACTIONS_RUNTIME_TOKEN
readonly approved_source=f1c2dfde45bdbe91425af6bb17d80e606703baac
readonly control="$RUNNER_TEMP/control"
readonly qualification="$control/qualification/termpane-publish"
readonly candidate="$RUNNER_TEMP/candidate"
readonly source="$RUNNER_TEMP/source"
readonly preview="$RUNNER_TEMP/preview"
readonly image='rust@sha256:4c4c8ec55be09785e6d6ac45ea9853525a73c15db74009100ac08952e773dfb8'
test "$(git -C "$source" rev-parse HEAD)" = "$SOURCE_SHA"
source_status="$(git -C "$source" status --porcelain=v1 --untracked-files=all --ignored=matching)"
test -z "$source_status"
mv "$source" "$control"
cd "$control"
python3 "$qualification/candidate_admission.py" "$CANDIDATE_SHA" "$CANDIDATE_PR"
git init "$candidate"
git -C "$candidate" fetch --depth 1 https://github.com/tailrocks/velnor-new.git "refs/pull/$CANDIDATE_PR/head"
test "$(git -C "$candidate" rev-parse FETCH_HEAD)" = "$CANDIDATE_SHA"
git -C "$candidate" checkout --detach "$CANDIDATE_SHA"
git clone https://github.com/tailrocks/termpane.git "$source"
git -C "$source" checkout --detach "$approved_source"
test "$(git -C "$source" rev-parse HEAD)" = "$approved_source"
test -z "$(git -C "$source" status --porcelain)"
git clone --no-local "$source" "$preview"
git -C "$preview" checkout --detach "$approved_source"
git -C "$preview" remote set-url origin https://github.com/tailrocks/termpane.git
mkdir "$RUNNER_TEMP/nextest" "$RUNNER_TEMP/release-plz" "$RUNNER_TEMP/cargo-cache"
fetch() {
  local url="$1" path="$2" sha="$3"
  curl -fsSL --max-filesize 104857600 --max-redirs 5 --proto-redir '=https' --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 -o "$path" "$url"
  printf '%s  %s\n' "$sha" "$path" | sha256sum -c -
}
fetch https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.151/cargo-nextest-0.9.151-x86_64-unknown-linux-gnu.tar.gz "$RUNNER_TEMP/nextest.tar.gz" 1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4
tar -xzf "$RUNNER_TEMP/nextest.tar.gz" -C "$RUNNER_TEMP/nextest" cargo-nextest
fetch https://github.com/release-plz/release-plz/releases/download/release-plz-v0.4.3/release-plz-x86_64-unknown-linux-musl.tar.gz "$RUNNER_TEMP/release-plz.tar.gz" 4a4cfa92ea2abaee02b40daa596a95ca2725816a6a32cb03a8f48ddead287681
tar -xzf "$RUNNER_TEMP/release-plz.tar.gz" -C "$RUNNER_TEMP/release-plz" release-plz
printf '%s  %s\n' 678330a1f03330fb375d15be8d6e094d0f65d2b70e2af417c69fcec78fa74466 "$RUNNER_TEMP/release-plz/release-plz" | sha256sum -c -
docker pull --platform linux/amd64 rust@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f
docker run --platform linux/amd64 --rm -v "$source:/source" -v "$RUNNER_TEMP/cargo-cache:/cargo-cache" -e CARGO_HOME=/cargo-cache -e RUSTUP_TOOLCHAIN=1.97.1 -w /source rust@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f cargo fetch --locked
python3 "$qualification/candidate_admission.py" "$CANDIDATE_SHA" "$CANDIDATE_PR"
python3 "$qualification/prepare_preview.py" "$preview" "$candidate" "$qualification/provisional-config.toml" "$CANDIDATE_SHA"
bash "$qualification/assert_candidate_cargo_config.sh" "$candidate"
docker pull --platform linux/amd64 "$image"
run_candidate_test() {
  local test_root="$RUNNER_TEMP/candidate-$1"
  local output
  local -a test_args=()
  mkdir -p "$test_root/cargo" "$test_root/home" "$test_root/target" "$test_root/output"
  case "$1" in
    supplied_consumer_intent_preview)
      output=intent.toml
      test_args=(-e VELNOR_TEST_PREVIEW_ROOT="$preview" -e VELNOR_TEST_PREVIEW_OUTPUT="$test_root/output/$output")
      ;;
    supplied_intent_renders_actual_upload_config)
      output=upload.toml
      test_args=(-v "$RUNNER_TEMP/intent.toml:/input-intent.toml:ro" -e VELNOR_TEST_UPLOAD_INTENT=/input-intent.toml \
        -e VELNOR_TEST_UPLOAD_PACKAGE=termpane -e VELNOR_TEST_UPLOAD_OUTPUT="$test_root/output/$output")
      ;;
    supplied_intent_renders_rust_verifier)
      output=rust-verifier.sh
      test_args=(-v "$RUNNER_TEMP/intent.toml:/input-intent.toml:ro" -e VELNOR_TEST_RUST_VERIFIER_INTENT=/input-intent.toml \
        -e VELNOR_TEST_RUST_VERIFIER_OUTPUT="$test_root/output/$output")
      ;;
    *) return 1 ;;
  esac
  docker run --platform linux/amd64 --rm --user "$(id -u):$(id -g)" \
    -v "$candidate:$candidate:ro" -v "$preview:$preview:ro" \
    -v "$RUNNER_TEMP/nextest:$RUNNER_TEMP/nextest:ro" \
    -v "$test_root:$test_root" -w "$candidate" \
    -e HOME="$test_root/home" -e CARGO_HOME="$test_root/cargo" \
    -e CARGO_TARGET_DIR="$test_root/target" \
    -e RUSTUP_HOME=/usr/local/rustup -e RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu -e RUSTUP_AUTO_INSTALL=0 \
    -e TMPDIR="$test_root/output" -e PATH="$RUNNER_TEMP/nextest:/usr/local/cargo/bin:/usr/bin:/bin" \
    "${test_args[@]}" \
    "$image" cargo nextest run --locked -p velnor-actions-orchestrator --lib -E "test($1)" --no-tests fail
  test ! -L "$test_root/output"
  test -d "$test_root/output"
  test ! -L "$test_root/output/$output"
  test -f "$test_root/output/$output"
  cp "$test_root/output/$output" "$RUNNER_TEMP/$output"
}
run_candidate_test supplied_consumer_intent_preview
run_candidate_test supplied_intent_renders_actual_upload_config
run_candidate_test supplied_intent_renders_rust_verifier
cp "$RUNNER_TEMP/intent.toml" "$RUNNER_TEMP/upload.toml" "$RUNNER_TEMP/release-plz/"
bash "$qualification/run_native_rehearsal.sh"

python3 "$qualification/candidate_admission.py" "$CANDIDATE_SHA" "$CANDIDATE_PR"
