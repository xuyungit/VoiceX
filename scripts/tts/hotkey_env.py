#!/usr/bin/env python3
"""Print the reading hotkeys bound in VoiceX as shell exports for the harness.

The drivers inject the default bindings (Option+Command+R to read,
Option+Command+T to translate and read). A maintainer who rebound either one
gets a harness that presses a key nothing listens to: no `hotkey_action` line,
no HUD, and every case "fails" for a reason that is not the product's. This
reads the bindings out of the settings row and prints what lib.sh expects:

    eval "$(scripts/tts/hotkey_env.py)"

Only the two hotkey fields are extracted, through sqlite's json_extract — the
settings row also holds API keys, and nothing here ever selects the row itself.

Storage format is "keyCode|modifiers|usesFn"; the key code space is laid out at
the top of src-tauri/src/hotkey/config.rs. Special keys are stored as their
macOS virtual keycode, which is what CGEvent wants. Letters are stored as ASCII
and digits as tagged ASCII (0x100 | '0'..'9'), so both are mapped back to their
ANSI keycode. Modifier-only and Fn bindings are refused: cgevent_key.py presses
a key under modifier flags, and those bindings have no such key.

Digits were plain ASCII once, where '1' (49) was indistinguishable from Space
(49). The app rewrites the unambiguous leftovers (50, 52, 57) the first time
the current build opens the settings; one still in the database means that has
not happened yet, and it is refused rather than guessed.

Usage:
    hotkey_env.py [--db PATH]
"""

import argparse
import os
import sqlite3
import sys

DEFAULT_DB = os.path.expanduser(
    "~/Library/Application Support/com.voicex.app/voicex.db"
)

# kVK_ANSI_* for the letters.
LETTER_KEYCODES = {
    "A": 0, "S": 1, "D": 2, "F": 3, "H": 4, "G": 5, "Z": 6, "X": 7, "C": 8,
    "V": 9, "B": 11, "Q": 12, "W": 13, "E": 14, "R": 15, "Y": 16, "T": 17,
    "O": 31, "U": 32, "I": 34, "P": 35, "L": 37, "J": 38, "K": 40, "N": 45,
    "M": 46,
}

# kVK_ANSI_0 .. kVK_ANSI_9, indexed by digit.
DIGIT_KEYCODES = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25]
DIGIT_KEY_CODE_TAG = 0x100

# Stored as the virtual keycode itself: Return, Tab, Space, Delete, Escape.
SPECIAL_KEYCODES = {36, 48, 49, 51, 53}

# Shift, Command, Option, Control (left and right), and Fn.
MODIFIER_KEY_CODES = {54, 55, 56, 58, 59, 60, 61, 62, 63}

# '2', '4', '9' from before digits were tagged.
LEGACY_DIGIT_KEY_CODES = {50, 52, 57}

# Internal modifier bits (hotkey/manager.rs), in the order cgevent_key.py
# presses them.
MODIFIER_BITS = [
    (0x1000, "control"),
    (0x0800, "option"),
    (0x0200, "shift"),
    (0x0100, "command"),
]

# (settings field, export prefix, default storage value)
BINDINGS = [
    ("ttsHotkeyConfig", "VOICEX_READ", "82|2304|0"),
    ("ttsTranslateHotkeyConfig", "VOICEX_TRANSLATE", "84|2304|0"),
]


def decode(field, stored):
    parts = stored.split("|")
    if len(parts) < 2:
        sys.exit(f"{field}: unexpected storage value {stored!r}")
    key_code, modifiers = int(parts[0]), int(parts[1])
    if len(parts) > 2 and parts[2] == "1":
        sys.exit(f"{field}: the binding uses Fn, which CGEvent injection cannot press")

    known = sum(bit for bit, _ in MODIFIER_BITS)
    if modifiers & ~known:
        sys.exit(f"{field}: unknown modifier bits in {modifiers:#06x}")
    mods = [name for bit, name in MODIFIER_BITS if modifiers & bit]
    return virtual_keycode(field, key_code), ",".join(mods)


def virtual_keycode(field, key_code):
    if key_code in SPECIAL_KEYCODES:
        return key_code
    if chr(key_code) in LETTER_KEYCODES:
        return LETTER_KEYCODES[chr(key_code)]
    digit = key_code - DIGIT_KEY_CODE_TAG - ord("0")
    if 0 <= digit <= 9:
        return DIGIT_KEYCODES[digit]
    if key_code in MODIFIER_KEY_CODES:
        sys.exit(
            f"{field}: key code {key_code} is a modifier-only or Fn binding, "
            "which CGEvent injection cannot press"
        )
    if key_code in LEGACY_DIGIT_KEY_CODES:
        sys.exit(
            f"{field}: key code {key_code} is a digit in the pre-migration format; "
            "start the current VoiceX build once so it rewrites the setting"
        )
    sys.exit(
        f"{field}: key code {key_code} is not a key VoiceX names; "
        "set the KEY/MODS variables by hand (see docs/tts-regression-testing.md)"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--db", default=DEFAULT_DB)
    args = parser.parse_args()

    if not os.path.exists(args.db):
        sys.exit(f"settings database not found: {args.db}")

    conn = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True, timeout=3)
    for field, prefix, default in BINDINGS:
        row = conn.execute(
            "select json_extract(value, ?) from user_config where key = 'app_settings'",
            (f"$.{field}",),
        ).fetchone()
        # An absent field is the default binding, not a missing one.
        stored = row[0] if row and row[0] else default
        key, mods = decode(field, stored)
        print(f"export {prefix}_KEY={key}")
        print(f"export {prefix}_MODS={mods}")
        print(f"# {field} = {stored}", file=sys.stderr)


if __name__ == "__main__":
    main()
