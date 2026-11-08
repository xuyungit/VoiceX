# Development

Building and running VoiceX from source. For using the app, see the
[README](../README.en.md) ([中文](../README.md)).

## Tech stack

| Layer | Technology |
|---|---|
| Frontend | Vue 3 · TypeScript · Naive UI · Vite |
| Desktop shell | Tauri 2 (Rust) |
| Audio capture | cpal · Opus (OggOpus) · 16 kHz mono |
| Sync server | Rust · Axum · SQLite |

## Prerequisites

- [Node.js](https://nodejs.org/) (LTS)
- [pnpm](https://pnpm.io/)
- [Rust](https://rustup.rs/) (stable)
- Tauri 2 system dependencies: [Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/)

## Getting started

```bash
# Install JS dependencies
pnpm install

# Start the web dev server
pnpm dev

# Start the desktop dev environment (Tauri)
pnpm tauri dev

# Production build (type check + frontend bundle, then the desktop app)
pnpm build
pnpm tauri build
```

## macOS permissions during development

VoiceX needs Microphone, Accessibility, and Input Monitoring; in the app, the
Overview setup card and drag helper walk through them (see
[permission-onboarding-2026-11-07.md](./permission-onboarding-2026-11-07.md)).
Two things differ for builds you make yourself:

- `pnpm tauri dev` runs a bare binary from `target/`, not an `.app` bundle, so
  macOS charges the permissions to the terminal that launched it. Grant them to
  that terminal app.
- Release builds refuse to start outside `/Applications/VoiceX.app`, so that
  permission grants stay attached to one stable path.

### Local signing (recommended)

macOS ties permission grants to the app's code signature. Without a stable
signature every rebuild looks like a different app and **all three permissions
have to be granted again**. Signing with a persistent local certificate keeps
them across rebuilds:

```bash
# One-time setup: create a local code-signing identity in your Keychain
pnpm mac:setup-signing

# Build, sign, and install to /Applications
pnpm mac:build-local
```

`mac:setup-signing` creates a self-signed certificate named "VoiceX Local Code
Signing" in your login keychain (once). `mac:build-local` builds a release,
signs it with that identity, and installs it to `/Applications` with the
quarantine flag removed. To produce only a DMG without installing, run
`pnpm tauri build --bundles dmg`.

> Local signing is for development and personal builds. Distribution builds
> should use an Apple Developer certificate and notarization.

## Windows build

No code signing is needed for local development. Build with PowerShell:

```powershell
.\scripts\Build-VoiceX.ps1
```

## Releasing

Installed copies update themselves from GitHub Releases (design:
[auto-update-design-2026-11-08.md](./auto-update-design-2026-11-08.md)). Only
**official builds** check for updates. These are release builds that merge
`src-tauri/tauri.updater.conf.json`, which holds the updater's public key and
endpoint. `pnpm tauri dev`, `mac:build-local`, and `Build-VoiceX.ps1` leave it
out, so those builds never check and are never replaced by a published version.

Keys (back up both offline):

| Key | Where | If lost |
|---|---|---|
| Updater signing key | `~/.tauri/voicex-updater.key` + `~/.tauri/voicex-updater.password`; GitHub secrets `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Installed copies can no longer auto-update; every user must reinstall by hand |
| "VoiceX Local Code Signing" certificate | login keychain | Every Mac user must grant Microphone, Accessibility, and Input Monitoring again |

Steps:

1. Bump the version in `package.json`, `src-tauri/Cargo.toml`, and
   `src-tauri/tauri.conf.json`, update `CHANGELOG.md`, commit, tag `vX.Y.Z`,
   and push the commit and the tag.
2. On the Apple Silicon Mac, with the tag checked out:
   ```bash
   pnpm mac:release vX.Y.Z --notes-file notes.md --publish
   ```
   This creates the release as a draft if it doesn't exist, builds and signs the app,
   and uploads the DMG, `VoiceX_X.Y.Z_aarch64.app.tar.gz`, its `.sig`, and
   `latest.json`. With `--publish` it then publishes the release; leave the flag
   off to check the draft first.
3. Publishing starts `.github/workflows/windows-release.yml`, which uploads the
   NSIS installer and its `.sig` and adds `windows-x86_64` to `latest.json`.
   Until it finishes, Windows copies see no update.

A release marked as a prerelease is never offered: the endpoint follows
`releases/latest`, which skips prereleases and drafts.

## README screenshots

README images are rendered from the real frontend with an in-memory backend
fixture; see [scripts/screenshots/README.md](../scripts/screenshots/README.md).

## Project structure

```
src/                 # Vue 3 frontend
  components/        #   Shared UI components
  views/             #   Route pages (Overview, History, Dictionary, settings, About)
  stores/            #   Pinia state management
  hud/               #   Lightweight HUD overlay
  permission-guide/  #   macOS drag-to-authorize helper window
src-tauri/           # Tauri (Rust) desktop shell
  src/               #   Tauri commands & core logic
  proto/             #   gRPC proto definitions (Google Cloud Speech)
  vendor/            #   Vendored dependencies (audiopus_sys, rdev)
sync-server/         # Self-hosted history sync server
tools/llm-bench/     # LLM correction benchmark
scripts/             # Build, signing, and screenshot helper scripts
docs/                # Design notes and research
```
