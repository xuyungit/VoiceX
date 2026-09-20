#!/usr/bin/env bash
#
# Phase-0 smoke test for selected-text reading (docs/tts_plan.md §5, 阶段 0).
#
# Drives a real application, selects text, injects the read hotkey (⌥⌘R) as
# synthetic key events, and asserts the structured log emitted by
# src-tauri/src/tts/ shows the whole chain: selection read -> speech start ->
# stop on the second press.
#
# This is the phase-0 stand-in for the harness described in §4.2. It is
# intentionally a single script with inline fixtures; phase 1 splits it into
# per-application drivers, a fixtures directory and a gate script.
#
# Prerequisites (residual manual item #1 in the plan):
#   - VoiceX is running, with stderr captured to a log file:
#       pnpm tauri dev 2>&1 | tee /tmp/voicex-tts-smoke.log
#   - The running VoiceX binary has Accessibility + Input Monitoring granted.
#   - The terminal running this script has Accessibility and Automation
#     (System Events, TextEdit, Safari) granted.
#
# Usage:
#   scripts/tts/smoke_phase0.sh [--log PATH] [--app textedit|safari|all]

set -euo pipefail

LOG_FILE="${VOICEX_LOG:-/tmp/voicex-tts-smoke.log}"
TARGET_APPS="all"

# Unique per run, and embedded in every fixture. Teardown closes documents and
# tabs *only* when they carry this marker: "the frontmost document" or "any tab
# named fixture.html" can just as easily be the user's own work, and closing it
# without saving is unrecoverable.
RUN_ID="$(uuidgen | tr -d '-' | tr 'A-Z' 'a-z' | cut -c1-12)"

# Long enough that speech is still running when the second hotkey press lands.
FIXTURE_TEXT="VoiceX selected text reading smoke test $RUN_ID. 这是一段用于验证跨应用朗读链路的中文测试文本。The quick brown fox jumps over the lazy dog, and then reads it back aloud."

# Rust reports `chars().count()`, i.e. Unicode scalars — the same unit Python's
# len() gives. Asserting on it proves we read the fixture and not, say, the
# address bar, which a bare "selection succeeded" check happily accepts.
EXPECTED_CHARS="$(python3 -c 'import sys; print(len(sys.argv[1]))' "$FIXTURE_TEXT")"

# How long the chain gets before we call it a failure.
SPEAK_TIMEOUT_S=10
STOP_TIMEOUT_S=5

while [ $# -gt 0 ]; do
  case "$1" in
    --log) LOG_FILE="$2"; shift 2 ;;
    --app) TARGET_APPS="$2"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

FAILURES=0
INVALID=0
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

info()    { printf '\033[36m==>\033[0m %s\n' "$*"; }
pass()    { printf '\033[32mPASS\033[0m %s\n' "$*"; }
fail()    { printf '\033[31mFAIL\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
invalid() { printf '\033[33mINVALID\033[0m %s\n' "$*"; INVALID=$((INVALID + 1)); }

# --- log helpers -------------------------------------------------------------

log_size() { wc -c < "$LOG_FILE" | tr -d ' '; }
log_since() { tail -c "+$(( $1 + 1 ))" "$LOG_FILE"; }

wait_for_event() {
  local offset="$1" pattern="$2" timeout_s="$3"
  local deadline=$(( $(date +%s) + timeout_s ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    if log_since "$offset" | grep -qE "$pattern"; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

# --- input injection ---------------------------------------------------------

# Keys go through cgevent_key.py, not AppleScript: VoiceX's hook taps at
# kCGHIDEventTap, which sits ahead of the session tap where System Events
# delivers, so AppleScript keystrokes are invisible to it.
INJECT="$(dirname "$0")/cgevent_key.py"

# Key code 15 is the physical R key. A rebound hotkey comes in through the same
# variables lib.sh reads (`eval "$(scripts/tts/hotkey_env.py)"`).
trigger_read_hotkey() {
  python3 "$INJECT" --key "${VOICEX_READ_KEY:-15}" --mods "${VOICEX_READ_MODS:-option,command}"
}

# Key code 0 is the physical A key.
select_all() {
  python3 "$INJECT" --key 0 --mods command
}

# --- application drivers -----------------------------------------------------

setup_textedit() {
  osascript >/dev/null <<APPLESCRIPT
tell application "TextEdit"
  activate
  set newDoc to make new document
  set text of newDoc to "$FIXTURE_TEXT"
end tell
APPLESCRIPT
  sleep 1
}

teardown_textedit() {
  # Match on the run marker, never on position. `close document 1 saving no`
  # would discard whatever document happens to be frontmost — including the
  # user's unsaved work.
  osascript >/dev/null <<APPLESCRIPT || true
tell application "TextEdit"
  repeat with i from (count of documents) to 1 by -1
    try
      if (text of document i) contains "$RUN_ID" then close document i saving no
    end try
  end repeat
end tell
APPLESCRIPT
}

SAFARI_FIXTURE=""
SAFARI_WINDOW_ID=""

setup_safari() {
  SAFARI_FIXTURE="$TMP_DIR/fixture-$RUN_ID.html"
  cat > "$SAFARI_FIXTURE" <<HTML
<!doctype html><meta charset="utf-8"><title>VoiceX TTS fixture</title>
<body><p>$FIXTURE_TEXT</p></body>
HTML
  # Two steps on purpose. Safari ignores AppleScript navigation to file:// URLs
  # (`set URL of …` leaves the tab on favorites://), so the page has to be
  # opened through Launch Services — but `open -a Safari FILE` drops a tab into
  # whichever window is frontmost, which would put the fixture among the user's
  # real tabs. Creating an empty window first makes that frontmost window ours.
  SAFARI_WINDOW_ID="$(osascript <<'APPLESCRIPT'
tell application "Safari"
  activate
  make new document
  return id of front window
end tell
APPLESCRIPT
)"
  sleep 1
  open -a Safari "$SAFARI_FIXTURE"
  sleep 3

  local url
  url="$(osascript -e 'tell application "Safari" to return URL of current tab of front window' 2>/dev/null || true)"
  case "$url" in
    *"fixture-$RUN_ID.html") ;;
    *)
      # Not a product failure — the driver never got the fixture on screen.
      echo "     driver could not load the fixture (front tab URL: ${url:-none})"
      return 1
      ;;
  esac

  # A real mouse click, not System Events' accessibility click: only genuine
  # mouse events move keyboard focus out of the address bar and into the page,
  # and without that the following Cmd-A selects the URL instead of the text.
  local bounds x y
  bounds="$(osascript -e 'tell application "Safari" to return bounds of front window')"
  x="$(echo "$bounds" | awk -F', *' '{print int(($1+$3)/2)}')"
  y="$(echo "$bounds" | awk -F', *' '{print int($2+170)}')"
  python3 "$(dirname "$0")/cgevent_click.py" --at "$x,$y"
  sleep 0.5
}

teardown_safari() {
  # Close the fixture *tabs*, never a window. Closing a window whose current
  # tab is the fixture also destroys every other tab in it — and those can be
  # the user's, since Safari is free to place our page wherever it likes.
  # Closing the last tab of a window closes that window on its own, which is
  # the only case where losing a window is correct.
  osascript >/dev/null <<APPLESCRIPT || true
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

  # Then drop the window we created, but only if nothing but blank tabs is left
  # in it — never a window we did not open, and never one holding real pages.
  if [ -n "$SAFARI_WINDOW_ID" ]; then
    osascript >/dev/null 2>&1 <<APPLESCRIPT || true
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

# --- the actual check --------------------------------------------------------

run_case() {
  local label="$1" setup="$2" teardown="$3"

  info "$label: preparing the application and selecting the fixture text"
  # A driver that cannot stage the fixture proves nothing about the product, so
  # it is reported as invalid rather than as a product failure — but it still
  # keeps the run from passing (plan §4.3: a broken P0 driver never yields GO).
  if ! "$setup"; then
    invalid "$label: driver could not stage the test"
    "$teardown"
    return
  fi
  select_all
  sleep 0.5

  local offset
  offset="$(log_size)"

  info "$label: injecting the read hotkey"
  trigger_read_hotkey

  if wait_for_event "$offset" "event=selection_ok" "$SPEAK_TIMEOUT_S"; then
    local line
    line="$(log_since "$offset" | grep -E 'event=selection_ok' | tail -1)"
    if echo "$line" | grep -q "chars=$EXPECTED_CHARS"; then
      pass "$label: selection read succeeded ($EXPECTED_CHARS chars)"
    else
      fail "$label: read the wrong text (expected chars=$EXPECTED_CHARS)"
    fi
    echo "$line" | sed 's/^/     /'
  else
    fail "$label: no successful selection read"
    log_since "$offset" | grep -E 'event=(selection_err|selection_start|hotkey_action)' | sed 's/^/     /' || true
    "$teardown"
    return
  fi

  if wait_for_event "$offset" 'event=speak_started' "$SPEAK_TIMEOUT_S"; then
    pass "$label: speech started"
  else
    fail "$label: speech never started"
    log_since "$offset" | grep -E 'event=speak' | sed 's/^/     /' || true
    "$teardown"
    return
  fi

  local stop_offset
  stop_offset="$(log_size)"
  info "$label: injecting the read hotkey again to stop"
  trigger_read_hotkey

  # Two events, because they answer different questions. `speak_stopped` says
  # the controller accepted a stop for the right reason; since `stop` became
  # non-blocking it fires when the request is queued, so on its own it cannot
  # tell a stop that worked from one that was merely accepted.
  # `speak_cancelled` is the engine reporting that the audio actually ended.
  if ! wait_for_event "$stop_offset" 'event=speak_stopped reason=hotkey' "$STOP_TIMEOUT_S"; then
    fail "$label: the second press never reached the controller"
    log_since "$stop_offset" | grep -E 'event=' | sed 's/^/     /' || true
  elif wait_for_event "$stop_offset" 'event=speak_cancelled' "$STOP_TIMEOUT_S"; then
    pass "$label: speech stopped on the second press"
  else
    fail "$label: the stop was accepted but the engine never went silent"
    log_since "$stop_offset" | grep -E 'event=' | sed 's/^/     /' || true
  fi

  "$teardown"
  sleep 0.5
}

# --- preflight ---------------------------------------------------------------

if [ ! -f "$LOG_FILE" ]; then
  echo "Log file not found: $LOG_FILE" >&2
  echo "Start VoiceX with:  pnpm tauri dev 2>&1 | tee $LOG_FILE" >&2
  exit 2
fi

if ! grep -q 'Selected-text reading hotkey' "$LOG_FILE"; then
  echo "The log does not show a VoiceX startup with selected-text reading enabled." >&2
  echo "Restart VoiceX and make sure its stderr goes to $LOG_FILE." >&2
  exit 2
fi

info "Log file: $LOG_FILE"

case "$TARGET_APPS" in
  textedit) run_case "TextEdit" setup_textedit teardown_textedit ;;
  safari)   run_case "Safari"   setup_safari   teardown_safari ;;
  all)
    run_case "TextEdit" setup_textedit teardown_textedit
    run_case "Safari"   setup_safari   teardown_safari
    ;;
  *) echo "Unknown --app value: $TARGET_APPS" >&2; exit 2 ;;
esac

echo
if [ "$INVALID" -gt 0 ]; then
  invalid "$INVALID case(s) never ran; the result is inconclusive"
  exit 1
fi
if [ "$FAILURES" -eq 0 ]; then
  pass "phase 0 smoke test passed"
  exit 0
fi
fail "$FAILURES check(s) failed"
exit 1
