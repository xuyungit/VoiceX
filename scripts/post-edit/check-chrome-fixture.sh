#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/voicex-post-edit-chrome.XXXXXX")"
fixture_pid=""
cleanup() {
  if [ -n "$fixture_pid" ]; then kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true; fi
  rm -rf "$fixture_dir"
}
trap cleanup EXIT
kind="${1:-text}"
case "$kind" in input|text|editable|password) ;; *) exit 2 ;; esac
session="${3:-read}"
case "$session" in read) query="" ;; session) query="?observe=1" ;; *) exit 2 ;; esac
full_tree=0
case "${2:-}" in full) full_tree=1; set -- --force-renderer-accessibility ;; "") set -- ;; *) exit 2 ;; esac
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --user-data-dir="$fixture_dir/profile" --no-first-run --no-default-browser-check "$@" "file://$PWD/scripts/post-edit/browser-fixture.html$query#$kind" > "$fixture_dir/browser.log" 2>&1 &
fixture_pid=$!
sleep 3
VOICEX_POST_EDIT_FIXTURE_PID="$fixture_pid" VOICEX_POST_EDIT_FIXTURE_KIND="$kind" VOICEX_POST_EDIT_BROWSER_SESSION="$session" VOICEX_POST_EDIT_EXPECT_READABLE="$full_tree" cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::tests::live_browser_fixture -- --ignored --nocapture --test-threads=1
