#!/usr/bin/env bash
#
# Capture the VoiceX HUD window to a PNG, by its on-screen rectangle.
#
# The rectangle comes from System Events, so this needs no window id and works
# for the dev binary, which has no bundle identifier and is therefore invisible
# to tools that address applications by one.
#
# `screencapture` needs Screen Recording for the *calling* process. A shell
# without it fails with "could not create image from rect" — that is a missing
# grant, not a missing HUD (which is reported separately, exit 1).
#
# Usage:
#   scripts/tts/hud_shot.sh OUT.png

set -uo pipefail

out="${1:?usage: hud_shot.sh OUT.png}"

geo="$(osascript -e 'tell application "System Events" to tell (first application process whose name is "voicex" or name is "VoiceX") to tell (first window whose name is "VoiceX HUD") to return {position, size}' 2>/dev/null)" || geo=""
if [ -z "$geo" ]; then
  echo "no HUD window on screen" >&2
  exit 1
fi

IFS=', ' read -r x y w h <<<"$geo"
if ! screencapture -R "$x,$y,$w,$h" -x -o "$out"; then
  echo "screencapture failed: the calling process has no Screen Recording grant" >&2
  exit 3
fi
echo "$out rect=$x,$y,$w,$h"
