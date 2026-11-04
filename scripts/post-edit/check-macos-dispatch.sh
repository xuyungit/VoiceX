#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/voicex-post-edit-dispatch.XXXXXX")"
trap 'rm -rf "$fixture_dir"' EXIT
output="${1:-$fixture_dir/dispatch-samples.json}"
case "$output" in /*) ;; *) output="$PWD/$output" ;; esac
mkdir -p "$(dirname "$output")"
swiftc -swift-version 5 scripts/post-edit/session-fixture.swift -o "$fixture_dir/fixture"
VOICEX_POST_EDIT_SESSION_FIXTURE="$fixture_dir/fixture" VOICEX_POST_EDIT_DISPATCH_SAMPLES="$output" cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::macos_tests::live_macos_dispatch_samples -- --ignored --nocapture --test-threads=1
node scripts/post-edit/summarize-dispatch.mjs "$output"
