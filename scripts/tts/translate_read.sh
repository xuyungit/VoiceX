#!/usr/bin/env bash
#
# End-to-end cases for translate-and-read (docs/translate-read-requirements).
#
# Drives the real hotkey from a TextEdit fixture and judges the outcome by what
# the feature leaves behind: a history row with mode "translate_read" on the
# happy path, and no row at all when the stage refused, was cancelled, or the
# read was stopped. The structured log is not needed, so this runs against an
# app started from any terminal — but the screen must be unlocked, because a
# locked console neither activates TextEdit nor delivers the injected keys.
#
# Usage:
#   scripts/tts/translate_read.sh [--db PATH] [--case success|toolong|cancel|stop|all]
#
# Cases:
#   success  short Chinese sentence → one translate_read row, no audio path
#   toolong  > 3000 chars → refused before the LLM call, no row
#   cancel   Esc while the LLM request is in flight → no row
#   stop     second hotkey press during speech → row exists (the translation
#            finished), speech is cut short; judged by ear, the row is reported
#
# The translate hotkey must be at its default (Option+Command+T) and
# "翻译并朗读" enabled in Reading settings. Injection needs Accessibility for
# the calling terminal (see cgevent_key.py).

set -uo pipefail

_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$_DIR/lib.sh"

DB="${VOICEX_DB:-$HOME/Library/Application Support/com.voicex.app/voicex.db}"
CASES="success"
KEYCODE_T=17
KEYCODE_ESC=53
ROW_TIMEOUT_S=40

while [ $# -gt 0 ]; do
  case "$1" in
    --db) DB="$2"; shift 2 ;;
    --case) CASES="$2"; shift 2 ;;
    -h|--help) sed -n '3,24p' "$0"; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ "$CASES" = all ] && CASES="success toolong cancel stop"

[ -r "$DB" ] || { echo "history database not readable: $DB" >&2; exit 2; }

max_rowid() { sqlite3 "$DB" "select coalesce(max(rowid),0) from history_record;"; }

screen_locked() {
  ioreg -n Root -d1 -a 2>/dev/null | grep -A1 IOConsoleLocked | grep -q '<true/>'
}

fixture_for() {
  case "$1" in
    success) printf '翻译朗读测试 %s：今天天气很好，我们去公园散步吧。' "$RUN_ID" ;;
    toolong) printf '翻译朗读测试 %s %s' "$RUN_ID" "$(python3 -c 'print("这是一段很长的文字。"*320)')" ;;
    cancel)  printf '翻译朗读测试 %s %s' "$RUN_ID" "$(python3 -c 'print("这是一段需要较长时间翻译的文字，用来测试取消。"*100)')" ;;
    stop)    printf '翻译朗读测试 %s：这是一段足够长的文字，用来验证第二次按下热键会停止朗读。我们会在朗读开始后再次触发同一个热键，然后确认朗读被打断。' "$RUN_ID" ;;
    *) return 1 ;;
  esac
}

open_fixture() {
  open -a TextEdit
  app_ready TextEdit || return 1
  osa 10 >/dev/null <<APPLESCRIPT
tell application "TextEdit"
  activate
  set newDoc to make new document
  set text of newDoc to "$1"
end tell
APPLESCRIPT
  sleep 1
  click_front_window_center TextEdit || true
}

close_fixture() {
  # Match on the run marker, never on position (see lib.sh RUN_ID).
  osa 10 >/dev/null <<APPLESCRIPT || true
tell application "TextEdit"
  repeat with i from (count of documents) to 1 by -1
    try
      if (text of document i) contains "$RUN_ID" then close document i saving no
    end try
  end repeat
end tell
APPLESCRIPT
}

# Poll for a history row newer than $1; prints the rowid or nothing.
wait_for_row() {
  local before="$1" deadline=$(( $(date +%s) + ROW_TIMEOUT_S )) cur
  while [ "$(date +%s)" -le "$deadline" ]; do
    cur="$(max_rowid)"
    if [ "$cur" -gt "$before" ]; then echo "$cur"; return 0; fi
    sleep 1
  done
  return 1
}

row_field() { sqlite3 "$DB" "select $2 from history_record where rowid=$1;"; }

run_case() {
  local name="$1" fixture before rowid front t0 mode
  fixture="$(fixture_for "$name")" || { fail "$name: unknown case"; return; }
  # ${#fixture} counts bytes under the C locale a non-interactive shell may
  # have, which would make the 3000-character cap look wrong in the output.
  info "case $name (run $RUN_ID, $(python3 -c 'import sys; print(len(sys.argv[1]))' "$fixture") chars)"

  if screen_locked; then invalid "$name: the screen is locked"; return; fi
  before="$(max_rowid)"
  open_fixture "$fixture" || { invalid "$name: could not open the TextEdit fixture"; return; }

  front="$(frontmost_bundle_id)"
  if [ "$front" != "com.apple.TextEdit" ]; then
    invalid "$name: TextEdit is not frontmost ($front); not injecting into another app"
    close_fixture; return
  fi

  select_all; sleep 0.4
  inject_key "$KEYCODE_T" option,command
  t0=$(date +%s)

  case "$name" in
    cancel) sleep 1.2; inject_key "$KEYCODE_ESC"; note "Esc sent 1.2 s after the hotkey" ;;
    stop)   sleep 9; inject_key "$KEYCODE_T" option,command; note "second hotkey sent 9 s after the first" ;;
  esac

  rowid="$(wait_for_row "$before")" || rowid=""
  close_fixture

  case "$name" in
    success|stop)
      if [ -z "$rowid" ]; then fail "$name: no history row within ${ROW_TIMEOUT_S}s"; return; fi
      mode="$(row_field "$rowid" mode)"
      if [ "$mode" != "translate_read" ]; then fail "$name: row $rowid has mode '$mode'"; return; fi
      [ "$(row_field "$rowid" llm_invoked)" = 1 ] || fail "$name: llm_invoked is not 1"
      [ -z "$(row_field "$rowid" audio_path)" ] || fail "$name: audio_path should be empty"
      [ "$(row_field "$rowid" original_text)" = "$fixture" ] || fail "$name: original_text is not the selection"
      pass "$name: row $rowid after $(( $(date +%s) - t0 ))s, model=$(row_field "$rowid" llm_model_name)"
      note "text: $(row_field "$rowid" text)"
      ;;
    toolong|cancel)
      if [ -n "$rowid" ]; then
        fail "$name: unexpected history row $rowid (mode=$(row_field "$rowid" mode))"
      else
        pass "$name: no history row, as required"
      fi
      ;;
  esac
}

for c in $CASES; do run_case "$c"; done

if [ "$FAILURES" -gt 0 ] || [ "$INVALID" -gt 0 ]; then
  info "failures=$FAILURES invalid=$INVALID"; exit 1
fi
info "all cases passed"
