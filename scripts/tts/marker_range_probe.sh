#!/usr/bin/env bash
#
# Live probe of the WebKit marker-range selection read (docs/tts_plan.md §5.1).
#
# Stages a Safari page whose main text is selected, gives the web area keyboard
# focus, then runs the ignored `live_marker_range_read` test against Safari's
# pid. The test walks the same layers the app does — `AXSelectedText`,
# attribute enumeration, then `AXSelectedTextMarkerRange` resolved through
# `AXStringForTextMarkerRange` — and prints what each one answered.
#
# Why a probe and not a harness case: the AX layer is asked directly by the test
# binary, so this needs neither a running VoiceX nor its log. It exists to
# answer one question quickly when a WebKit page fails to read — does the
# marker path work against *this* Safari, on *this* page shape — with the
# fixture below standing in for a page (paragraphs, inline image, code block,
# table).
#
# Prerequisites:
#   - The calling terminal has Accessibility and Automation (Safari, System
#     Events) granted. The test binary inherits the terminal's trust.
#   - Safari installed. Any existing Safari windows are left alone; the
#     fixture gets its own window, which is closed again on teardown.
#
# Usage:
#   scripts/tts/marker_range_probe.sh [--fixture PATH]
#
#   --fixture  Use this HTML page instead of the built-in one. The page must
#              select the text to read itself (see the fixture below for how);
#              the probe only clicks the window centre to give it focus.

set -euo pipefail

_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$_SCRIPT_DIR/lib.sh"

REPO_ROOT="$(cd "$_SCRIPT_DIR/../.." && pwd)"
FIXTURE_SRC=""

while [ $# -gt 0 ]; do
  case "$1" in
    --fixture) FIXTURE_SRC="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

TMP_DIR="$(mktemp -d)"
SAFARI_WINDOW_ID=""

# --- fixture -----------------------------------------------------------------

# Selects `#target` on load and again 150 ms after any click: the focus click
# collapses the selection, and the marker read needs both focus and selection.
write_marker_fixture() {
  cat > "$1" <<'HTML'
<!doctype html><meta charset="utf-8"><title>VoiceX marker-range fixture</title>
<style>body{font:16px/1.6 -apple-system;max-width:720px;margin:40px auto}pre{background:#eee;padding:8px}</style>
<h1>标题 Heading</h1>
<article id="target">
<p>第一段：传感器持续测量校准的调研报告，包含中文与 English 混排。</p>
<p>Second paragraph with <strong>bold</strong> and <a href="#">a link</a>, plus an image <img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="dot"> inline.</p>
<pre><code>fn main() { println!("hello"); }</code></pre>
<table><tr><td>单元格 A</td><td>Cell B</td></tr><tr><td>C</td><td>D</td></tr></table>
<p>Last paragraph inside the selection END</p>
</article>
<p>Trailing paragraph outside the selection.</p>
<script>
  const select = () => {
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.selectAllChildren(document.getElementById('target'));
    document.title = 'selected ' + sel.toString().length + ' chars';
  };
  window.addEventListener('load', select);
  document.addEventListener('click', () => setTimeout(select, 150));
</script>
HTML
}

# --- Safari driver (same shape as p0_survey.sh) ------------------------------

setup_safari() {
  local fixture="$TMP_DIR/fixture-$RUN_ID.html"
  if [ -n "$FIXTURE_SRC" ]; then
    cp "$FIXTURE_SRC" "$fixture"
  else
    write_marker_fixture "$fixture"
  fi
  open -a Safari
  app_ready Safari || return 1

  # New window first so `open -a Safari FILE` lands among our tabs, not the
  # user's (Safari ignores AppleScript navigation to file:// URLs).
  SAFARI_WINDOW_ID="$(osa 25 <<'APPLESCRIPT'
tell application "Safari"
  activate
  make new document
  return id of front window
end tell
APPLESCRIPT
)"
  sleep 1
  open -a Safari "$fixture"
  sleep 3

  local url
  url="$(osa1 10 'tell application "Safari" to return URL of current tab of front window')"
  case "$url" in
    *"fixture-$RUN_ID.html") ;;
    *) note "could not load the fixture (front tab: ${url:-none})"; return 1 ;;
  esac

  click_front_window_center "Safari"
  sleep 0.5
  note "front tab after focus click: $(osa1 10 'tell application "Safari" to return name of current tab of front window')"
}

teardown_safari() {
  # Fixture tabs only, matched on RUN_ID — never a positional "front tab".
  osa 25 >/dev/null <<APPLESCRIPT || true
tell application "Safari"
  repeat with wi from (count of windows) to 1 by -1
    try
      repeat with ti from (count of tabs of window wi) to 1 by -1
        if (URL of tab ti of window wi) contains "fixture-$RUN_ID.html" then
          close tab ti of window wi
        end if
      end repeat
    end try
  end repeat
end tell
APPLESCRIPT

  if [ -n "$SAFARI_WINDOW_ID" ]; then
    osa 25 >/dev/null <<APPLESCRIPT || true
tell application "Safari"
  repeat with wi from (count of windows) to 1 by -1
    if (id of window wi) is $SAFARI_WINDOW_ID then
      set hasContent to false
      try
        repeat with t in tabs of window wi
          if (URL of t) is not "favorites://" then set hasContent to true
        end repeat
      end try
      if not hasContent then close window wi
    end if
  end repeat
end tell
APPLESCRIPT
    SAFARI_WINDOW_ID=""
  fi
}

cleanup() {
  teardown_safari
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

# --- run ----------------------------------------------------------------------

info "building the test binary"
(cd "$REPO_ROOT/src-tauri" && cargo test --no-run --lib 2>&1 | grep -v '^\s*Compiling' | tail -3)

info "staging the fixture in Safari"
setup_safari || { fail "Safari driver could not stage the fixture"; exit 1; }

SAFARI_PID="$(pgrep -x Safari | head -1)"
[ -n "$SAFARI_PID" ] || { fail "Safari is not running"; exit 1; }

info "running live_marker_range_read against Safari (pid $SAFARI_PID)"
# The system-wide focused-element query answers kAXErrorCannotComplete for a
# binary launched from a shell, so the test is pointed at Safari's application
# element instead; the app itself keeps the system-wide read.
if (cd "$REPO_ROOT/src-tauri" && VOICEX_LIVE_PID="$SAFARI_PID" \
      cargo test --lib -- --ignored live_marker_range_read --nocapture 2>&1 \
      | grep -v '^\s*\(Compiling\|Finished\|Running\)'); then
  pass "marker-range read produced text"
else
  fail "marker-range read failed; see the output above"
  exit 1
fi
