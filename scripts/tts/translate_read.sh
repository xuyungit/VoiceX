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
#   scripts/tts/translate_read.sh [--db PATH] [--case success|long|toolong|cancel|stop|all]
#
# Cases:
#   success  short Chinese sentence → one translate_read row, no audio path
#   long     ~2900 chars, a long selection → one translate_read row whose
#            translation is not cut short (no fixed output-token cap upstream)
#   toolong  > 5000 chars → refused before the LLM call, no row
#   cancel   Esc while the LLM request is in flight → no row (Esc goes out
#            CANCEL_ESC_DELAY_S after the hotkey, see below)
#   stop     second hotkey press during speech → row exists (the translation
#            finished), speech is cut short; judged by ear, the row is reported
#
# "翻译并朗读" must be enabled in Reading settings. The hotkey injected is the
# default Option+Command+T; for another binding, first run
# `eval "$(scripts/tts/hotkey_env.py)"` (see lib.sh). Injection needs
# Accessibility for the calling terminal (see cgevent_key.py).
#
# A reading hotkey pressed while a session is active stops that session instead
# of starting another (tts/controller.rs), so every case first makes sure no
# read is in progress, and stops its own read once judged: the long fixture
# would otherwise speak for about ten minutes and swallow the next case's hotkey.

set -uo pipefail

_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$_DIR/lib.sh"

DB="${VOICEX_DB:-$HOME/Library/Application Support/com.voicex.app/voicex.db}"
CASES="success"
KEYCODE_ESC=53
ROW_TIMEOUT_S=40
# The cancel case must land its Esc while the LLM reply is still pending:
# after the selection read (tens of milliseconds) and before the fastest
# reply seen. Cerebras answered the 2320-character fixture in 1.1 s, so an
# Esc at 1.2 s stopped the speech of an already-saved translation instead.
CANCEL_ESC_DELAY_S=0.5

while [ $# -gt 0 ]; do
  case "$1" in
    --db) DB="$2"; shift 2 ;;
    --case) CASES="$2"; shift 2 ;;
    -h|--help) sed -n '3,24p' "$0"; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ "$CASES" = all ] && CASES="success long toolong cancel stop"

[ -r "$DB" ] || { echo "history database not readable: $DB" >&2; exit 2; }

# The app holds write transactions on the same database while a case runs;
# without a busy timeout sqlite3 gives up at once with "database is locked"
# and the empty answer breaks the numeric comparisons below.
sql() { sqlite3 -cmd ".timeout 3000" "$DB" "$1"; }

max_rowid() { sql "select coalesce(max(rowid),0) from history_record;"; }

screen_locked() {
  ioreg -n Root -d1 -a 2>/dev/null | grep -A1 IOConsoleLocked | grep -q '<true/>'
}

# Stop a read that is still going. A HUD that only lingers after a session
# ended hides by itself within a couple of seconds, so give it that long first:
# the hotkey sent to an idle app would start a read of whatever is selected.
stop_if_reading() {
  wait_hud_hidden 3 && return 0
  trigger_translate_hotkey
  if wait_hud_hidden 5; then
    note "stopped the read still in progress"
  else
    note "HUD still visible 5 s after the stop hotkey"
  fi
}

# Distinct numbered sections instead of one repeated sentence, so a translation
# that stops early is visible as missing section numbers, not as fewer copies.
long_fixture_body() {
  python3 - <<'PY'
para = ("桥梁健康监测系统在过去一年里积累了大量的时序数据，其中包括主梁挠度、支座反力、索力以及环境温度。"
        "为了从这些数据中识别结构状态的变化，我们先对原始信号做了去噪和重采样，然后按照温度区间分组，"
        "比较不同季节下同一测点的响应分布。结果显示，夏季高温时段主梁的竖向变形明显增大，但支座反力的变化幅度较小，"
        "说明温度效应主要通过材料膨胀影响几何形状，而不是通过边界条件的改变影响受力。下一步计划引入有限元模型进行反演，"
        "用实测的模态参数修正刚度系数，并把修正后的模型用于预测极端荷载工况下的安全裕度。")
parts, i = [], 1
while sum(len(p) for p in parts) < 2850:
    parts.append(f"第{i}节 " + para)
    i += 1
print("\n\n".join(parts)[:2850], end="")
PY
}

fixture_for() {
  case "$1" in
    success) printf '翻译朗读测试 %s：今天天气很好，我们去公园散步吧。' "$RUN_ID" ;;
    long)    printf '翻译朗读测试 %s %s' "$RUN_ID" "$(long_fixture_body)" ;;
    toolong) printf '翻译朗读测试 %s %s' "$RUN_ID" "$(python3 -c 'print("这是一段很长的文字。"*520)')" ;;
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

row_field() { sql "select $2 from history_record where rowid=$1;"; }

run_case() {
  local name="$1" fixture before rowid front t0 t_row mode translated
  fixture="$(fixture_for "$name")" || { fail "$name: unknown case"; return; }
  # ${#fixture} counts bytes under the C locale a non-interactive shell may
  # have, which would make the 5000-character cap look wrong in the output.
  info "case $name (run $RUN_ID, $(python3 -c 'import sys; print(len(sys.argv[1]))' "$fixture") chars)"

  if screen_locked; then invalid "$name: the screen is locked"; return; fi
  stop_if_reading
  before="$(max_rowid)"
  open_fixture "$fixture" || { invalid "$name: could not open the TextEdit fixture"; return; }

  front="$(frontmost_bundle_id)"
  if [ "$front" != "com.apple.TextEdit" ]; then
    invalid "$name: TextEdit is not frontmost ($front); not injecting into another app"
    close_fixture; return
  fi

  select_all; sleep 0.4
  trigger_translate_hotkey
  t0=$(date +%s)

  case "$name" in
    cancel) sleep "$CANCEL_ESC_DELAY_S"; inject_key "$KEYCODE_ESC"; note "Esc sent $CANCEL_ESC_DELAY_S s after the hotkey" ;;
    stop)   sleep 9; trigger_translate_hotkey; note "second hotkey sent 9 s after the first" ;;
  esac

  rowid="$(wait_for_row "$before")" || rowid=""
  t_row=$(date +%s)
  close_fixture
  stop_if_reading

  case "$name" in
    success|long|stop)
      if [ -z "$rowid" ]; then fail "$name: no history row within ${ROW_TIMEOUT_S}s"; return; fi
      mode="$(row_field "$rowid" mode)"
      if [ "$mode" != "translate_read" ]; then fail "$name: row $rowid has mode '$mode'"; return; fi
      [ "$(row_field "$rowid" llm_invoked)" = 1 ] || fail "$name: llm_invoked is not 1"
      [ -z "$(row_field "$rowid" audio_path)" ] || fail "$name: audio_path should be empty"
      [ "$(row_field "$rowid" original_text)" = "$fixture" ] || fail "$name: original_text is not the selection"
      if [ "$name" = long ]; then
        # The fixture numbers its sections and the last number must survive in
        # the tail of the output. Digits may come back as words (十二, twelve)
        # because the prompt asks for readable text, and the target language
        # is whatever Reading settings say, so accept any spelling.
        translated="$(row_field "$rowid" text)"
        if printf '%s' "$translated" | python3 -c '
import re, sys
t = sys.stdin.read(); tail = t[int(len(t) * 0.7):]
sys.exit(0 if re.search(r"12|十二|twelve", tail, re.I) else 1)'; then
          pass "$name: row $rowid after $(( t_row - t0 ))s, model=$(row_field "$rowid" llm_model_name), $(python3 -c 'import sys; print(len(sys.argv[1]))' "$translated") chars, section 12 present"
        else
          fail "$name: section 12 missing from the tail: …$(printf '%s' "$translated" | python3 -c 'import sys; print(sys.stdin.read()[-120:])')"
        fi
        return
      fi
      pass "$name: row $rowid after $(( t_row - t0 ))s, model=$(row_field "$rowid" llm_model_name)"
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
