#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/voicex-post-edit-sessions.XXXXXX")"
trap 'rm -rf "$fixture_dir"' EXIT
swiftc -swift-version 5 scripts/post-edit/session-fixture.swift -o "$fixture_dir/fixture"
if [ -n "${VOICEX_POST_EDIT_TEST_BINARY:-}" ]; then
    VOICEX_POST_EDIT_SESSION_FIXTURE="$fixture_dir/fixture" "$VOICEX_POST_EDIT_TEST_BINARY" post_edit::platform::macos_tests::live_macos_session_scenarios --ignored --nocapture --test-threads=1
else
    VOICEX_POST_EDIT_SESSION_FIXTURE="$fixture_dir/fixture" cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::macos_tests::live_macos_session_scenarios -- --ignored --nocapture --test-threads=1
fi
