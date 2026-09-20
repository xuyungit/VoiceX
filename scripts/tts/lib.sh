#!/usr/bin/env bash
#
# Shared harness machinery for the selected-text reading drivers.
#
# Sourced by scripts/tts/*.sh. Holds everything that is not application
# specific: structured-log tailing, CGEvent injection, the foreground assertion,
# and result bookkeeping. The per-application drivers live in the scripts that
# source this, one function pair (setup/teardown) each, so that a driver broken
# by an application update cannot take the others down with it (plan §4.2).

# --- run identity ------------------------------------------------------------

# Unique per run and embedded in every fixture. Teardown matches on this and
# never on position: "the frontmost document" or "the tab named fixture.html"
# is just as likely to be the user's own unsaved work, and closing that is
# unrecoverable. Phase 0 destroyed a document and a three-tab window learning
# this (plan §4.2).
RUN_ID="$(uuidgen | tr -d '-' | tr 'A-Z' 'a-z' | cut -c1-12)"

# --- output ------------------------------------------------------------------

FAILURES=0
INVALID=0

info()    { printf '\033[36m==>\033[0m %s\n' "$*"; }
pass()    { printf '\033[32mPASS\033[0m %s\n' "$*"; }
fail()    { printf '\033[31mFAIL\033[0m %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
# A driver that could not stage its fixture proves nothing about the product, so
# it is neither a pass nor a product failure — but it still keeps the run from
# being called complete (plan §4.3: a broken P0 driver never yields GO).
invalid() { printf '\033[33mINVALID\033[0m %s\n' "$*"; INVALID=$((INVALID + 1)); }
note()    { printf '     %s\n' "$*"; }

# --- structured log ----------------------------------------------------------

log_size()  { wc -c < "$LOG_FILE" | tr -d ' '; }
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

# Pull `key=value` out of a structured log line on stdin. Anchored on preceding
# whitespace so `chars=` cannot match inside `has_chars=`.
field() {
  sed -n "s/.*[[:space:]]$1=\([^[:space:]]*\).*/\1/p" | tail -1
}

# --- input injection ---------------------------------------------------------
#
# Keys go through CGEvent, never AppleScript: VoiceX taps at kCGHIDEventTap,
# which sits ahead of the session tap where System Events delivers, so
# AppleScript keystrokes are invisible to it (plan §4.2).

_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INJECT_KEY="$_SCRIPT_DIR/cgevent_key.py"
INJECT_CLICK="$_SCRIPT_DIR/cgevent_click.py"

KEYCODE_A=0
KEYCODE_R=15
KEYCODE_T=17
KEYCODE_W=13

inject_key() { python3 "$INJECT_KEY" --key "$1" --mods "${2:-}"; }

# The reading hotkeys are rebindable, and a harness pressing the default at an
# app bound to something else looks exactly like a broken product: no
# `hotkey_action` line, no HUD. `eval "$(scripts/tts/hotkey_env.py)"` exports
# the bindings the running app actually has; unset, these are the defaults.
trigger_read_hotkey() {
  inject_key "${VOICEX_READ_KEY:-$KEYCODE_R}" "${VOICEX_READ_MODS:-option,command}"
}
trigger_translate_hotkey() {
  inject_key "${VOICEX_TRANSLATE_KEY:-$KEYCODE_T}" "${VOICEX_TRANSLATE_MODS:-option,command}"
}
select_all() { inject_key "$KEYCODE_A" command; }

# --- AppleScript ---------------------------------------------------------------

# `timeout(1)` is not on every macOS; the AppleScript-level bound is the one
# that always applies, this is just a backstop for osascript itself wedging.
if command -v timeout >/dev/null 2>&1; then _timeout() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then _timeout() { gtimeout "$@"; }
else _timeout() { shift; "$@"; }
fi

# Run an AppleScript (on stdin) with a bounded per-event timeout.
#
# osascript waits 120 s per AppleEvent by default. An application that is cold
# starting does not pump events, so an unbounded driver turns a slow launch into
# a multi-minute stall — the first survey run lost eight minutes to exactly that
# in Preview and Terminal. A driver that cannot get an answer quickly should
# report `invalid` and move on.
osa() {
  local secs="$1"
  { echo "with timeout of $secs seconds"; cat; echo "end timeout"; } \
    | _timeout "$(( secs + 5 ))" osascript 2>/dev/null
}

osa1() { printf '%s\n' "$2" | osa "$1"; }

# Wait until an application is up and answering AppleEvents.
#
# Launching is not the same as being scriptable: `open -a` returns as soon as
# the process exists, well before it will answer.
app_ready() {
  local app="$1" deadline=$(( $(date +%s) + ${2:-40} ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    if [ -n "$(osa1 5 "tell application \"$app\" to return name")" ]; then
      return 0
    fi
    sleep 1
  done
  note "$app never became scriptable"
  return 1
}

# --- HUD ---------------------------------------------------------------------

# The HUD window is on screen only while a session runs, so its presence in the
# app's window list is the one idle signal readable from outside. Dev builds run
# as "voicex", packaged ones as "VoiceX".
hud_visible() {
  osa1 5 'tell application "System Events" to return name of windows of (first application process whose name is "voicex" or name is "VoiceX")' \
    | grep -q 'VoiceX HUD'
}

wait_hud_visible() {
  local deadline=$(( $(date +%s) + $1 ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    hud_visible && return 0
    sleep 0.3
  done
  return 1
}

wait_hud_hidden() {
  local deadline=$(( $(date +%s) + $1 ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    hud_visible || return 0
    sleep 0.5
  done
  return 1
}

# --- window geometry ---------------------------------------------------------

# Frontmost application's bundle identifier, or empty.
frontmost_bundle_id() {
  osa1 10 'tell application "System Events" to return bundle identifier of first application process whose frontmost is true'
}

# Front window title via System Events rather than the application's own
# dictionary. Useful for applications with no AppleScript object model (VS Code)
# and as a fallback when an application is not answering its own events.
front_window_title() {
  osa1 10 "tell application \"System Events\" to tell process \"$1\" to return name of front window"
}

# Poll until the front window's title contains a marker.
#
# Replaces a fixed sleep after `open -a`: how long an application takes to put
# a document on screen depends on whether it was already running, and a sleep
# long enough for a cold start wastes that time on every warm one.
wait_for_window_title() {
  local process_name="$1" marker="$2" deadline=$(( $(date +%s) + ${3:-30} )) title
  while [ "$(date +%s)" -le "$deadline" ]; do
    title="$(front_window_title "$process_name")"
    case "$title" in *"$marker"*) return 0 ;; esac
    sleep 1
  done
  note "$process_name front window never showed '$marker' (last: ${title:-none})"
  return 1
}

# Click the middle of an application's front window with a real mouse event.
#
# System Events' `click at` is an accessibility click: it activates the element
# under the point but does not move keyboard focus, so a following Cmd-A still
# goes wherever focus already was — in Safari, the address bar, which then
# "succeeds" by copying the URL (plan §4.2).
click_front_window_center() {
  local process_name="$1" geometry x y
  geometry="$(osa 10 <<APPLESCRIPT
tell application "System Events" to tell process "$process_name"
  set p to position of front window
  set s to size of front window
  return (item 1 of p as text) & "," & (item 2 of p as text) & "," & ¬
         (item 1 of s as text) & "," & (item 2 of s as text)
end tell
APPLESCRIPT
)"
  case "$geometry" in
    *,*,*,*) ;;
    *) note "could not read $process_name window geometry; skipping the focus click"; return 1 ;;
  esac

  x="$(echo "$geometry" | awk -F, '{print int($1 + $3 / 2)}')"
  y="$(echo "$geometry" | awk -F, '{print int($2 + $4 / 2)}')"
  python3 "$INJECT_CLICK" --at "$x,$y"
  sleep 0.5
}

# --- results -----------------------------------------------------------------
#
# One TSV row per application, rendered as a table at the end. The path
# distribution is the point of the survey; the pass/fail column only says
# whether the chain ran, and for `atleast` cases it deliberately says less than
# it looks (see the note printed under the table).

RESULTS_FILE=""

results_init() {
  RESULTS_FILE="$1"
  printf 'app\tresult\tpath\trole\tattr\tstatus\trange\tmarker\tms\tclipboard\tdetail\n' > "$RESULTS_FILE"
}

record_result() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$1" "$2" "${3:--}" "${4:--}" "${5:--}" "${6:--}" \
    "${7:--}" "${8:--}" "${9:--}" "${10:--}" "${11:--}" >> "$RESULTS_FILE"
}

print_summary() {
  echo
  info "P0 selection path distribution (RUN_ID $RUN_ID)"
  column -t -s $'\t' < "$RESULTS_FILE"
}
