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
