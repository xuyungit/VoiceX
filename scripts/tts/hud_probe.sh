#!/usr/bin/env bash
#
# One read, watched from outside: HUD and caption lifecycle probe.
#
# Stages a TextEdit fixture, starts a plain read or a translate-and-read with
# the real hotkey, lets it speak for a set time, presses the hotkey again if the
# read is still going, and reports when the HUD came and went. With --log it
# also prints the structured events the run produced, which is where the
# caption sequence and the end-of-read order are judged; with --shots it
# captures the HUD along the way for a look at what was actually drawn.
#
# What it judges by itself is deliberately little — the HUD appeared, and it
# was gone within five seconds of the end — because the rest is either in the
# log (compare with the expected sequences in docs/tts-regression-testing.md)
# or visual. A hotkey that never reached the app is INVALID, not FAIL: that is
# a missing Accessibility grant or a rebound hotkey, not the product.
#
# Usage:
#   scripts/tts/hud_probe.sh [--kind read|translate] [--fixture short|long|xlong]
#                            [--speak SECONDS] [--log PATH] [--shots DIR]
#
#   --kind     read (default) or translate
#   --fixture  short  one sentence, ends by itself within a few seconds
#              long   ~260 chars, several sentences (default)
#              xlong  ~2850 chars, just under the translate cap
#   --speak    seconds between the hotkey and the second press (default 20).
#              0 presses again half a second later: a stop before the first
#              sentence is heard. A read that ended by itself gets no second
#              press.
#   --log      VoiceX stderr, from `pnpm tauri dev 2>&1 | tee PATH`
#   --shots    directory for HUD captures (needs Screen Recording for the
#              calling shell, see hud_shot.sh)
#
# Run `eval "$(scripts/tts/hotkey_env.py)"` first if either hotkey was rebound.
# Must run in a foreground shell with Accessibility: injected keys from a
# background job are dropped without an error.

set -uo pipefail

_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$_DIR/lib.sh"

KIND="read"
FIXTURE="long"
SPEAK_S=20
LOG_FILE=""
SHOT_DIR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --kind) KIND="$2"; shift 2 ;;
    --fixture) FIXTURE="$2"; shift 2 ;;
    --speak) SPEAK_S="$2"; shift 2 ;;
    --log) LOG_FILE="$2"; shift 2 ;;
    --shots) SHOT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '3,37p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$KIND" in
  read)      press_hotkey() { trigger_read_hotkey; };      label="直接朗读测试" ;;
  translate) press_hotkey() { trigger_translate_hotkey; }; label="翻译朗读测试" ;;
  *) echo "Unknown kind: $KIND" >&2; exit 2 ;;
esac

if [ -n "$LOG_FILE" ] && [ ! -f "$LOG_FILE" ]; then
  echo "Log file not found: $LOG_FILE" >&2
  echo "Start VoiceX with:  pnpm tauri dev 2>&1 | tee $LOG_FILE" >&2
  exit 2
fi
[ -z "$SHOT_DIR" ] || mkdir -p "$SHOT_DIR"

# --- fixtures ----------------------------------------------------------------

PARAGRAPH="桥梁健康监测系统在过去一年里积累了大量的时序数据，其中包括主梁挠度、支座反力、索力以及环境温度。为了从这些数据中识别结构状态的变化，我们先对原始信号做了去噪和重采样，然后按照温度区间分组，比较不同季节下同一测点的响应分布。结果显示，夏季高温时段主梁的竖向变形明显增大，但支座反力的变化幅度较小，说明温度效应主要通过材料膨胀影响几何形状，而不是通过边界条件的改变影响受力。下一步计划引入有限元模型进行反演，用实测的模态参数修正刚度系数，并把修正后的模型用于预测极端荷载工况下的安全裕度。"

# Distinct numbered sections instead of one repeated paragraph, so a caption or
# a translation that lost its place shows up as a wrong section number.
xlong_body() {
  python3 - "$PARAGRAPH" <<'PY'
import sys
para, parts, i = sys.argv[1], [], 1
while sum(len(p) for p in parts) < 2850:
    parts.append(f"第{i}节 " + para); i += 1
print("\n\n".join(parts)[:2850], end="")
PY
}

case "$FIXTURE" in
  short) fixture="$label $RUN_ID：今天天气很好，我们去公园散步吧。" ;;
  long)  fixture="$label $RUN_ID。$PARAGRAPH" ;;
  xlong) fixture="$label $RUN_ID $(xlong_body)" ;;
  *) echo "Unknown fixture: $FIXTURE" >&2; exit 2 ;;
esac

close_fixture() {
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

# Stop a read that is still going. A HUD that only lingers after a session
# ended hides by itself within a couple of seconds, so give it that long first:
# the hotkey sent to an idle app would start a read of whatever is selected.
stop_if_reading() {
  wait_hud_hidden 3 && return 0
  press_hotkey
  if wait_hud_hidden 5; then
    note "stopped the read still in progress"
  else
    note "HUD still visible 5 s after the stop hotkey"
  fi
}

shot() {
  [ -n "$SHOT_DIR" ] || return 0
  "$_DIR/hud_shot.sh" "$SHOT_DIR/${KIND}_${FIXTURE}_$1.png" 2>&1 | sed "s/^/     $(date +%H:%M:%S) shot /"
}

# --- run ---------------------------------------------------------------------

chars="$(python3 -c 'import sys; print(len(sys.argv[1]))' "$fixture")"
info "$KIND / $FIXTURE ($chars chars, run $RUN_ID), second press after ${SPEAK_S}s"

stop_if_reading
open -a TextEdit
app_ready TextEdit || { invalid "TextEdit is not scriptable"; exit 2; }
osa 10 >/dev/null <<APPLESCRIPT
tell application "TextEdit"
  activate
  set newDoc to make new document
  set text of newDoc to "$fixture"
end tell
APPLESCRIPT
sleep 1
click_front_window_center TextEdit || true
front="$(frontmost_bundle_id)"
if [ "$front" != "com.apple.TextEdit" ]; then
  invalid "TextEdit is not frontmost ($front); not injecting into another app"
  close_fixture
  exit 2
fi

select_all; sleep 0.4
offset=0
[ -z "$LOG_FILE" ] || offset="$(log_size)"
press_hotkey
note "hotkey sent at $(date +%H:%M:%S)"

if [ "$SPEAK_S" = 0 ]; then
  # Unconditional: the point is a stop before anything is on screen, so there
  # is no HUD to wait for.
  sleep 0.5
  press_hotkey
  note "second press sent at $(date +%H:%M:%S)"
else
  if wait_hud_visible 5; then
    pass "HUD on screen at $(date +%H:%M:%S)"
  elif [ -n "$LOG_FILE" ] && ! log_since "$offset" | grep -q 'event=hotkey_action'; then
    invalid "no hotkey_action in the log: the injected key never reached VoiceX"
    close_fixture
    exit 2
  else
    fail "HUD did not appear within 5 s of the hotkey"
  fi

  deadline=$(( $(date +%s) + SPEAK_S ))
  n=0
  while [ "$(date +%s)" -lt "$deadline" ]; do
    sleep 1; n=$((n + 1))
    # Dense at the start, where the layout settles; sparse afterwards.
    if [ "$n" -le 4 ] || [ $((n % 4)) -eq 0 ]; then shot "$(printf %03d "$n")"; fi
  done

  if hud_visible; then
    press_hotkey
    note "second press sent at $(date +%H:%M:%S)"
  else
    note "HUD already gone at $(date +%H:%M:%S): the read ended by itself"
  fi
fi

if wait_hud_hidden 5; then
  pass "HUD hidden at $(date +%H:%M:%S)"
else
  fail "HUD still visible 5 s after the end of the read"
  press_hotkey
fi
close_fixture

if [ -n "$LOG_FILE" ]; then
  info "events of this run"
  log_since "$offset" | grep -oE 'event=.*' | sed 's/^/     /'
  if ! log_since "$offset" | grep -q 'event=hotkey_action'; then
    invalid "no hotkey_action in the log: the injected key never reached VoiceX"
  fi
fi

[ "$INVALID" -eq 0 ] || exit 2
[ "$FAILURES" -eq 0 ] || exit 1
