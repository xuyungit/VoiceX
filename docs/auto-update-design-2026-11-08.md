# In-app update: design (2026-11-08)

> Status: implemented. Decisions since the first draft: only official release
> builds check for updates (development and local builds never do), and
> Windows ships the NSIS installer only (no MSI). The release steps are in
> [development.md](./development.md#releasing).

Goal: VoiceX notices when a new release is out and installs it from inside the
app. With only a few users, it should cost as little as possible to build and to
run: no server, no paid service, and few changes to how releases are made today.

## 1. How releases work today

| Item | Current state |
|---|---|
| Distribution | GitHub Releases on the public repo `xuyungit/VoiceX`; tag `vX.Y.Z` on `main` |
| Cadence | About weekly (8 releases from v0.17.0 on 2026-10-01 to v0.18.1 on 2026-11-07) |
| Audience | 0–2 downloads per asset per release |
| macOS | Apple Silicon only. Built **locally**, signed with the self-signed "VoiceX Local Code Signing" certificate, **not notarized**, and the DMG is uploaded by hand |
| macOS runtime guard | Release builds won't start outside `/Applications/VoiceX.app` (`src-tauri/src/lib.rs:41`) |
| Windows | `.github/workflows/windows-release.yml` builds when a release is **published** (`tauri-action@v0.6.2`) and attaches unsigned NSIS `-setup.exe` and `.msi` |
| Version source | Kept in three files: `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` |
| Update awareness | None. The About page shows only the current version |

How users upgrade now: they watch GitHub, download the DMG or EXE, and on macOS
also deal with Gatekeeper because the app isn't notarized.

## 2. Options

| Option | What it does | Cost | Verdict |
|---|---|---|---|
| A. Check only | Call the GitHub `releases/latest` API, compare versions, show a banner that opens the release page | Lowest (about 100 lines) | Users still download and install by hand, and on macOS still meet Gatekeeper |
| **B. Tauri updater + static `latest.json` on GitHub Releases** | Official `tauri-plugin-updater`. It downloads a signed package and replaces the app in place | Low: one plugin, one key pair, a release-script step. **No server** | **Recommended** |
| C. Self-hosted update server or CrabNebula Cloud | Dynamic endpoint, channels, staged rollout | Medium to high, plus ongoing upkeep | Too much for this user base |

Option B costs only a little more than A, but users get a real one-click
upgrade. On macOS the experience is better than a manual install: the updater
downloads with its own HTTP client, so the new bundle gets **no quarantine
attribute** and Gatekeeper never prompts on updates. Only the first install
still needs the manual steps.

## 3. Recommended design (Option B)

### 3.1 How it works

```
Release:   build → sign the update package with the updater key (minisign)
           → upload {installer, update package, .sig, latest.json} to the GitHub Release

Client:    GET https://github.com/xuyungit/VoiceX/releases/latest/download/latest.json
           → compare its version with the running version
           → if newer: show the notes, the user clicks "Install"
           → download, check the signature against the pubkey built into the app
           → replace the app and restart
```

`releases/latest/download/<asset>` is a stable GitHub redirect to the newest
**non-prerelease, non-draft** release. Marking a release as a prerelease keeps
it away from users at no extra cost.

`latest.json` (the static manifest format the plugin reads):

```json
{
  "version": "0.18.2",
  "notes": "…release body…",
  "pub_date": "2026-11-10T08:00:00Z",
  "platforms": {
    "darwin-aarch64":  { "url": "https://github.com/xuyungit/VoiceX/releases/download/v0.18.2/VoiceX_0.18.2_aarch64.app.tar.gz", "signature": "<contents of .sig>" },
    "windows-x86_64":  { "url": "https://github.com/xuyungit/VoiceX/releases/download/v0.18.2/VoiceX_0.18.2_x64-setup.exe",     "signature": "<contents of .sig>" }
  }
}
```

### 3.2 One-time setup

1. **Generate the updater key pair**: `pnpm tauri signer generate -w ~/.tauri/voicex-updater.key`
   - The public key goes into `tauri.conf.json`.
   - The private key and its password go into the GitHub secrets
     `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, and
     into local env vars for the macOS build.
   - **Back up the private key offline.** If it is lost, no installed copy can
     auto-update again; every user would have to reinstall by hand.
   - Back up the "VoiceX Local Code Signing" certificate (.p12) as well. Updates
     keep macOS permissions only if the new bundle is signed with the **same**
     certificate (see 3.6). This is already true today, but auto-update makes it
     matter more.

2. **Config**: all updater config lives in a **release-only overlay**,
   `src-tauri/tauri.updater.conf.json`, merged with
   `tauri build --config src-tauri/tauri.updater.conf.json`:
   ```json
   "bundle": { "createUpdaterArtifacts": true },
   "plugins": {
     "updater": {
       "pubkey": "<public key>",
       "endpoints": ["https://github.com/xuyungit/VoiceX/releases/latest/download/latest.json"],
       "windows": { "installMode": "passive" }
     }
   }
   ```
   Keeping it out of `tauri.conf.json` does two things:
   - Builds without the private key still work. With `createUpdaterArtifacts`
     in the main config, `mac:build-local`, `Build-VoiceX.ps1`, and builds by
     other contributors would all fail.
   - **Only official builds check for updates.** The app registers the plugin
     only when the build is release profile *and* the updater config is
     present (`src-tauri/src/updater.rs`). A `pnpm tauri dev` build or a local
     release build is never offered, or replaced by, a published version.

3. **Dependencies**: `tauri-plugin-updater = "2"`, registered at runtime from
   `updater::init` (not on the builder, because without its config the plugin
   fails to start). No JS plugin is needed (see 3.3). The plugin's reqwest
   0.12 is built without system-proxy support, so `Cargo.toml` also names
   reqwest 0.12 with the `system-proxy` feature to turn it on.

### 3.3 App side (Rust commands, small UI)

Keep the logic in Rust instead of using `@tauri-apps/plugin-updater` from JS:

- The startup and periodic checks run while the main window may be hidden (the
  app lives in the tray).
- An install must not start **during a recording or dictation session**. Rust
  can check `AppState.is_recording` (and TTS playback) directly.
- No new capability permissions are needed for the webview.

New module `src-tauri/src/updater.rs` (about 150 lines):

| Command / task | Behavior |
|---|---|
| `check_for_update()` | `app.updater()?.check()`. Returns `{ current, version, notes, date }` or `None`, and caches the `Update` in state |
| `install_update()` | Refuses while recording. Calls `download_and_install` and emits `update://progress` events (downloaded/total), then `app.restart()` |
| Background check | First check about 15 s after start, then every 24 h, if the "Check for updates automatically" setting is on (default **on**). When an update is found, emit `update://available` and add a tray menu item "Update to vX.Y.Z…" |

Errors are not hidden (per the repo rule). A failed **manual** check shows its
error on the About page next to an "Open release page" link. A failed
**background** check is logged at `warn` and not shown. "Platform not found in
manifest" counts as "no update" (see 3.4 for why that case is normal).

UI (Vue):
- **About page**: "Check for updates" button and status: up to date / vX.Y.Z
  available + notes + "Install and restart" / progress bar / error.
- **Sidebar**: a dot on the About item when an update is available.
- Settings: a single toggle, "Check for updates automatically".
- Nothing is forced or installed silently. The user always clicks Install. A
  "Skip this version" option is not needed at this scale.
- New i18n keys in both locales (`scripts/check-i18n.mjs` checks them).

### 3.4 Release process changes

**Windows (CI)**, in `windows-release.yml` for the `tauri-action` step:
```yaml
env:
  GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
with:
  releaseId: ...
  tagName: ...
  args: --config src-tauri/tauri.updater.conf.json
  updaterJsonPreferNsis: true
```
`tauri-action` uploads the `.sig` files and `latest.json`. If the release
already has a `latest.json`, it keeps that file's `platforms` and adds its own
(confirmed in the v0.6.2 source, `src/upload-version-json.ts`). It rewrites
`notes` from its `releaseBody` input, so the workflow passes the release body
there; otherwise the notes would be emptied.

The Windows bundle target is set to `nsis` only in `src-tauri/tauri.windows.conf.json`.

**macOS (local)**: a new script, `scripts/release-macos.sh vX.Y.Z`, replaces
the manual upload:
1. Check that the tag matches the version in all three version files.
2. Build `pnpm tauri build --config src-tauri/tauri.updater.conf.json` with the
   signing env vars set. This produces the DMG, `VoiceX.app.tar.gz`, and
   `VoiceX.app.tar.gz.sig`.
3. Rename the package to `VoiceX_X.Y.Z_aarch64.app.tar.gz` (and its `.sig`).
4. Write `latest.json` with the `darwin-aarch64` entry, using the release body
   from `gh release view` as `notes`.
5. Run `gh release upload` for the DMG, the `.app.tar.gz`, its `.sig`, and
   `latest.json`. Do this **before** publishing the release, or as part of
   `gh release create`, so the Windows CI finds and merges the file.

**Timing gap**: from publish until the Windows CI finishes (about 15–25 min),
`latest.json` lists only `darwin-aarch64`. Windows clients see "platform not
found", which counts as no update, and they pick it up on a later check. Nothing
breaks.

The macOS script merges in the same way: it keeps any platforms already in the
release's `latest.json` for the same version, so the order of the two halves
doesn't matter, and re-running either is safe.

### 3.5 Platform notes

**macOS**
- The plugin replaces the bundle at its current path, `/Applications/VoiceX.app`,
  so the install-path guard is still satisfied. If that folder isn't writable by
  the user, the plugin asks for admin rights.
- The update package is fetched by the app's own HTTP client, so it has no
  quarantine attribute and no Gatekeeper prompt. The DMG is still needed for the
  first install.
- There is no Intel build today. `latest.json` has no `darwin-x86_64`, so
  nothing would be offered there.

**Windows**
- NSIS defaults to `currentUser` install mode, so updates need no UAC prompt.
  `passive` shows a progress window, closes VoiceX, installs, and relaunches.
- The download has no Mark-of-the-Web, so SmartScreen usually doesn't block the
  unsigned installer. Antivirus may still flag it now and then.
- **MSI**: an MSI install updated through the NSIS package could leave two
  copies (Program Files and `%LOCALAPPDATA%`). MSI downloads are almost zero, so
  MSI is no longer published. Tell the few MSI users once to uninstall it and
  install the EXE.

**Network**
- GitHub may be slow or blocked for some users, for example in mainland China.
  The plugin's reqwest client uses the system proxy and `HTTPS_PROXY`. If that
  isn't enough, the manual check shows the error and the release-page link.
- Optional later step: mirror `latest.json` and the packages on our own server
  and add it as a **second endpoint**. The plugin tries endpoints in order, and
  the mirrored manifest's URLs would point at the mirror. Skip this until
  someone asks for it.

### 3.6 Keeping macOS permissions across updates

Accessibility, Input Monitoring, and Microphone grants are tied to the app's
designated requirement. For a self-signed app that is the bundle ID plus the
signing certificate's hash. An update signed with the **same** local
certificate keeps all three grants, the same as a reinstall with
`mac:build-local` today. This needs checking once on a real machine during the
first update test.

## 4. Rollout

1. **Bootstrap release** (for example v0.18.2): the first version with the
   updater. Users install it by hand once. Say so in the release notes.
2. **Test update**: before publishing the next release (for example v0.18.3),
   check on one Mac and one Windows machine:
   - It finds the update, downloads, checks the signature, installs, and relaunches.
   - macOS permissions are still granted after the update, with no Gatekeeper prompt.
   - Windows needs no UAC and leaves no duplicate install.
   - Install is refused during a recording.
   - A tampered `.sig` is rejected (use a throwaway manifest, for example a gist
     URL in a temporary build).
3. Add the new steps to `docs/development.md` (key location, `release-macos.sh`,
   secrets).

## 5. Effort and files

About 1–1.5 days in total.

| Area | Files |
|---|---|
| Config | `src-tauri/tauri.conf.json` (pubkey, endpoint; NSIS-only Windows target), new `src-tauri/tauri.updater.conf.json`, `src-tauri/Cargo.toml` |
| Rust | new `src-tauri/src/updater.rs`, `src-tauri/src/lib.rs` (plugin + commands + background task), `src-tauri/src/tray.rs` (menu item), settings (auto-check toggle) |
| Frontend | `src/views/About.vue`, sidebar dot, settings toggle, i18n files |
| Release | `.github/workflows/windows-release.yml`, new `scripts/release-macos.sh`, `docs/development.md` |
| Secrets | GitHub: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; offline backups of the updater key and the macOS signing certificate |
