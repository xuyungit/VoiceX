# Permission onboarding (macOS drag-to-authorize, Windows microphone)

Date: 2026-11-07

## Problem

VoiceX only works once the OS lets it hear the microphone, see the global
hotkey and type into other apps. On macOS each of these is a separate privacy
grant the user has to find in System Settings. Before this change VoiceX
checked two of them at startup and only wrote the result to the log; the UI
never said anything, and the hotkey listener never recovered when the user
granted access later.

## What VoiceX needs

### macOS

| Permission | Used for | Detect | Grant path |
|---|---|---|---|
| Microphone | `cpal` capture | `AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio` | System prompt (`requestAccessForMediaType:`). The Microphone list has no "+" button, so an app can only get onto it through the prompt; if the user declined, they turn the switch on in `Privacy_Microphone`. |
| Accessibility | 1) the hotkey is an *active* event tap (`rdev::grab`, `kCGEventTapOptionDefault`) that swallows the hotkey; 2) synthesized paste/typing (`CGEventPost`); 3) reading the selection (AX API) | `AXIsProcessTrusted()` | Drag `VoiceX.app` into `Privacy_Accessibility` (or "+") and switch it on |
| Input Monitoring | Receiving global keyboard events | `CGPreflightListenEventAccess()` | Drag into `Privacy_ListenEvent` (or "+"); the first failed tap creation also adds VoiceX to the list |

Not needed: Screen Recording, Full Disk Access, Automation (VoiceX sends no
Apple Events). `NSAppleEventsUsageDescription` in `Info.plist` is unused, and
Accessibility has no usage-description key at all: macOS words that prompt
itself.

Open question: Apple's guidance is that an active tap needs Accessibility and a
listen-only tap needs Input Monitoring. Whether an active tap *also* needs
Input Monitoring has not been verified on a clean machine. Until it is,
VoiceX asks for both (it always has); if a clean-machine test shows
Accessibility alone is enough, drop the Input Monitoring row.

Grants are keyed on the app's code-signing designated requirement. The fixed
`VoiceX Local Code Signing` identity and the `/Applications/VoiceX.app`
install check keep them valid across updates. If the signature ever changes,
the old list entry still shows as switched on but no longer matches; the fix
is to remove it with "−" and add the app again, which the drag icon turns
into one gesture.

### Windows

No equivalent of the macOS privacy lists for what VoiceX does: the
low-level keyboard hook, `SendInput`, the clipboard and UI Automation all
work without a grant.

- Microphone: Settings › Privacy & security › Microphone. If "Microphone
  access" or "Let desktop apps access your microphone" is off, capture fails
  or returns silence. Desktop (unpackaged) apps cannot be switched on
  individually. The state is in the registry under
  `…\CapabilityAccessManager\ConsentStore\microphone` (HKLM `Value` for the
  device-wide switch, HKCU `Value` for the user switch, HKCU
  `microphone\NonPackaged\Value` for desktop apps). Open the page with
  `ms-settings:privacy-microphone`. Nothing to drag.
- UIPI: a normal-privilege VoiceX cannot read from or type into an elevated
  window. This is not a grant the user can give; the selection reader already
  reports it as `TargetElevated`.

## The drag-to-authorize interaction

1. VoiceX opens the exact privacy page:
   `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`
   (or `Privacy_ListenEvent`, `Privacy_Microphone`).
2. A small always-on-top helper window appears just below the System Settings
   window (found through `CGWindowListCopyWindowInfo`, owner
   `com.apple.systempreferences`; window bounds need no Screen Recording
   permission). It shows the VoiceX icon and one line of instruction.
   The main window is hidden while the helper is open: pressing the helper
   icon activates VoiceX, and AppKit then orders the main window above
   System Settings, covering the list (seen in testing). It is shown again
   when the grant lands or the helper is closed.
3. Pressing on the icon starts a real AppKit file drag of
   `/Applications/VoiceX.app` (`NSDraggingItem` with a file URL, via the
   `drag` crate). The Accessibility and Input Monitoring lists accept dropped
   `.app` bundles exactly like "+" → choose app; macOS may ask for Touch ID or
   the password, and the entry is added switched on.
4. A Rust watcher polls the permission every 500 ms. When it flips to
   granted, the helper closes and the main window comes back to the front.
5. The hotkey listener retries the event tap every second, so the hotkey
   starts working without a restart. If both grants are in place but the
   tap still cannot be created, the card offers "Restart VoiceX".

## Where the prompt lives: Overview card, not a first-run wizard

Decision: a conditional **"Finish setup" card at the top of Overview**, plus a
**warning dot on the Overview sidebar item**. No separate onboarding wizard.

Why:

- Permissions can break at any time, not just on first run (the user turns
  a switch off, the signature changes, macOS resets TCC). A wizard runs once;
  a card driven by live detection catches every case with the same UI.
- VoiceX opens on Overview with the main window visible, so a first-run user
  lands on the card anyway: it already acts as the first-run step.
- It is the standard "setup checklist / system-status banner" pattern for
  utilities: inline, dismissed by fixing the cause rather than by a close
  button, and invisible once everything is fine.
- The sidebar dot covers users who are on another page; the card is one
  click away.

Layout:

- Placed between the page header and the stat sections; it renders nothing
  when everything is granted, so the stats keep the layout tuned for the
  default 900 × 650 window. While the card is visible, the Reading section
  may move below the fold, which is acceptable for a temporary state.
- One row per permission that is not yet granted: icon, name, one-line
  reason ("Needed for the hotkey and for typing into other apps"), and one
  primary action. Granted rows are hidden, so the card shrinks as the user
  makes progress.
- Actions: Microphone not yet asked → "Allow" (system prompt). Microphone
  declined → "Open Settings". Accessibility / Input Monitoring → "Grant"
  (opens the page plus the drag helper). Grants complete but the hotkey
  listener is down → "Restart VoiceX".
- Uses the warning accent colour, not error red: nothing has failed yet.

Not done here (possible follow-ups): a tray-menu warning item when the main
window is closed, and fixing the stale `Info.plist` usage strings.

## Implementation map

- `src-tauri/src/permissions/` — status snapshot, open-pane, microphone
  request, drag helper window, grant watcher (macOS and Windows).
- `src-tauri/src/hotkey/manager.rs` — listener retries the tap and exposes
  whether it is live.
- `src/stores/permissions.ts`, `src/components/PermissionSetupCard.vue`,
  Overview and Sidebar wiring.
- `src/permission-guide/` — the helper window page (separate Vite entry,
  like the HUD).
