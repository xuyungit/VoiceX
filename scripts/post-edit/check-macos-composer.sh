#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/voicex-composer.XXXXXX")"
fixture_pid=""
cleanup() {
  if [ -n "$fixture_pid" ]; then kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true; fi
  rm -rf "$fixture_dir"
}
trap cleanup EXIT
test_binary="${VOICEX_POST_EDIT_TEST_BINARY:?compile the lib tests and set VOICEX_POST_EDIT_TEST_BINARY}"
for scenario in ${VOICEX_POST_EDIT_COMPOSER_SCENARIOS:-enter button pseudo slate rewrite shift-enter mixed chip}; do
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    --user-data-dir="$fixture_dir/$scenario" --no-first-run --no-default-browser-check \
    --force-renderer-accessibility "file://$PWD/scripts/post-edit/composer-fixture.html#$scenario" \
    > "$fixture_dir/$scenario.log" 2>&1 &
  fixture_pid=$!
  sleep 3
  VOICEX_POST_EDIT_FIXTURE_PID="$fixture_pid" VOICEX_POST_EDIT_COMPOSER_SCENARIO="$scenario" \
    "$test_binary" post_edit::platform::macos_tests::live_chromium_composer --exact --ignored --nocapture --test-threads=1
  kill "$fixture_pid" 2>/dev/null || true
  wait "$fixture_pid" 2>/dev/null || true
  fixture_pid=""
done
