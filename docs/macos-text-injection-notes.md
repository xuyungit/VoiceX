# macOS Text Injection Notes

Last updated: 2026-03-31

## Background

VoiceX supports a `typing` text injection mode on macOS using `enigo`.

Recent changes in post-processing increased the chance that the final injected text contains one
or more newlines. That made two symptoms more visible in `typing` mode:

- characters near a newline were sometimes missing at their original position and appeared later
- Chinese IMEs could be woken up during injection and sometimes received stray `a` / `aa`

These symptoms were observed intermittently and were easier to reproduce with multiline text.

## Current Understanding

### 1. Why text is chunked to 20 characters

On macOS, `enigo` uses `CGEventKeyboardSetUnicodeString` internally. Its own macOS implementation
splits text into chunks of at most 20 Unicode scalars before posting events. This limit is not a
VoiceX-specific heuristic; it comes from the behavior of the underlying macOS injection path.

Implication:

- keeping a 20-character chunk size in macOS `typing` mode is reasonable
- changing the size without replacing the underlying injector is risky

### 2. Why newlines are the main trigger

The strongest correlation we found is that failures are tied to newlines in the injected text.

There appear to be two newline-related failure modes:

- If a newline is embedded inside a Unicode text chunk, macOS can deliver the prefix but mishandle
  the suffix. This matches the earlier symptom where some trailing characters disappeared from
  their original position and showed up later.
- If a chunk starts with a newline, `enigo` takes a special workaround path. Based on the observed
  `a` / `aa` artifacts and the upstream macOS implementation, this path appears capable of waking
  the active IME and leaking the underlying virtual key semantics into the target app.

We cannot yet prove the exact internal Quartz/IME behavior end-to-end, but the newline correlation
is strong enough to guide the workaround and future design.

### 3. Why the earlier inter-chunk delay was probably not the real fix

We previously tried adding a small delay between chunks. That may have changed timing, but it does
not explain why the problem clusters around multiline text.

Current view:

- the delay was an exploratory mitigation, not a root-cause fix
- newline handling is the more important variable

## Work Completed

The current macOS workaround is intentionally conservative:

- single-line text in `typing` mode still uses `enigo`
- multiline text in `typing` mode falls back to pasteboard injection

This avoids two unacceptable behaviors for now:

- IME activation during multiline injection
- treating newline as a real `Enter` key, which could accidentally submit a message in chat/dialog
  UIs

## Why We Rejected the "real Enter key" workaround

One explored idea was to replace newline characters with a real `Return` key press.

We decided not to ship that as the default behavior because it changes semantics:

- in a multiline text field, `Return` may insert a newline
- in a chat box, dialog, command field, or confirm-style input, `Return` may send/submit/confirm

That makes it unsafe as a general-purpose text injection strategy.

## Options Considered

### Option A: Keep patching `enigo` behavior

Examples:

- add more chunk timing tweaks
- special-case newline placement
- synthesize alternate control sequences

Assessment:

- low implementation cost
- increasingly fragile
- likely to keep breaking on IME-specific or app-specific behavior

### Option B: macOS-native local injector via Accessibility APIs

High-level idea:

- detect the focused accessibility element
- inspect whether it supports direct text/value insertion
- inject text into the target control as text, not as simulated keyboard events

Pros:

- better semantic match for text insertion
- avoids many IME/keyboard-event problems
- feasible in a Rust/Tauri desktop app

Cons:

- support varies by target app/control
- custom editors and browser-based editors may not expose useful AX insertion hooks
- still needs fallbacks

Assessment:

- best next engineering direction
- should be designed as a layered injector, not a universal replacement promise

### Option C: Build a full macOS input-method style integration

High-level idea:

- use system-level input method infrastructure instead of app-driven injection

Pros:

- closest to the native text input stack

Cons:

- much larger product and implementation scope
- changes distribution, permissions, and user setup expectations
- overkill for the current VoiceX product shape

Assessment:

- technically possible
- not recommended as the next step

## Recommended Direction

Short term:

- keep the multiline `typing` fallback to pasteboard on macOS
- continue using `typing` for single-line text

Medium term:

- prototype a local macOS injector based on Accessibility APIs
- keep pasteboard and keyboard simulation as fallbacks

Recommended injector order for macOS:

1. Native/local Accessibility-based insertion when supported
2. Pasteboard injection
3. Keyboard simulation as a narrow fallback

## Open Questions

- Which target apps used by VoiceX users expose enough AX support for direct insertion?
- Can we reliably detect "safe for direct AX insertion" vs "must fallback" without noticeable lag?
- Do we want an advanced setting to control multiline fallback behavior for power users?

## Summary

The current bug is not just "typing mode is flaky". The evidence points much more specifically to
newline handling on the macOS Unicode event injection path.

For now, the safest behavior is:

- single-line text: `typing`
- multiline text: pasteboard fallback

The proper longer-term fix is a native/local macOS injector with layered fallbacks.
