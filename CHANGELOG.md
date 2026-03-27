# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.7.0] - 2026-03-27

### Added
- **Bilingual UI localization** with Chinese and English support across the main window, tray menu, and HUD, including a follow-system language option.
- **History re-transcription** for existing recordings.
- **Batch ASR mode for Coli** to improve local offline recognition workflows.

### Changed
- Google Speech-to-Text re-transcription now uses the synchronous Recognize API for recordings up to 60 seconds.
- Tuned Coli VAD defaults for more reliable local ASR behavior.
- Refined the main window chrome by removing the redundant brand header and reducing top spacing.

### Fixed
- Increased the overall re-transcription timeout to 300 seconds.
- Re-transcription details now show the original ASR and LLM model names more clearly.

## [0.6.1] - 2026-03-27

### Fixed
- Fixed Windows local `coli` ASR startup when Node.js receives a `\\?\` verbatim path from canonicalized command discovery.
- Fixed Windows local `coli` ASR sessions that could stay active after stdin closed, delaying finalization.
- Suppressed transient console windows when the packaged Windows app launches local `coli` CLI processes.

## [0.6.0] - 2026-03-26

### Added
- **Local offline ASR** via [Coli](https://www.npmjs.com/package/@marswave/coli) — supports SenseVoice and Whisper models for fully offline speech recognition.
- **Qwen Realtime ASR** (DashScope) — Alibaba Cloud streaming ASR provider with `qwen3-asr-flash-realtime` model.
- **Google Cloud Speech-to-Text V2** — gRPC-based streaming with Chirp 3, multi-language support, and phrase boost.
- **Translation mode** — double-tap gesture triggers English translation via LLM, configurable trigger window.
- Case-insensitive keyword substitution rules (exact and contains patterns).
- History records now persist ASR and LLM model names for traceability.
- Windows tray icon improvements.
- LLM benchmark tool with Gemini and OpenAI Responses API support.

### Changed
- Improved history detail dialog with refined metadata layout.
- Better Windows CLI discovery and ASR finalization stabilization.

## [0.5.0] - 2026-03-05

### Added
- **Qwen LLM provider** (Alibaba DashScope) — `qwen3.5-flash` as default model.
- **LLM benchmark tool** (`tools/llm-bench/`) for evaluating ASR correction quality across providers.

### Changed
- Updated default models for Volcengine and Qwen providers.
- Tuned bilingual ASR defaults.

## [0.4.0] - 2026-01-24

### Added
- **Cross-device history sync** — self-hosted sync server (`sync-server/`) with HMAC shared-secret authentication.
- Device usage statistics tracking and per-device aggregation.
- History record deletion with sync propagation.
- Build info display in About page.
- Open recordings folder from UI.

## [0.3.0] - 2026-01-10

### Added
- **Online hotword sync** with Volcengine self-learning platform (bidirectional).
- Force-download hotwords from remote.
- Hotword sync diagnostics.
- LLM history context injection (optional — uses last N inputs for better correction).
- Hotkey permission checks on macOS.

### Improved
- Audio device listing with current default indication.
- Error handling for hotword service responses.

## [0.2.0] - 2026-01-07

### Added
- **Global hotkey system** with push-to-talk and hands-free modes.
- **Audio capture** with Opus encoding (OggOpus, 16 kHz mono).
- **HUD overlay** — real-time transcription display, mode indicators, countdown timer.
- **Multi-provider LLM architecture** — Volcengine (Doubao) and OpenAI support.
- Tray icon with show/quit menu.
- Preferred audio input device selection.
- ASR final-result fallback timeout.
- Application icon and branding.
- Windows and macOS cross-platform build support.

## [0.1.0] - 2026-01-04

### Added
- Initial release — core voice input pipeline.
- **Volcengine ASR** (Doubao Speech) — streaming speech recognition with hot-word boosting, ITN, punctuation, and DDC.
- **LLM correction** — post-ASR text correction with customizable prompt templates and `{{DICTIONARY}}` placeholder.
- **Text injection** — clipboard-based paste (with backup/restore) and simulated typing (Windows SendInput, macOS enigo).
- **Dictionary** — plain-text hot-word list sent to ASR and LLM.
- **Post-processing** — trailing punctuation removal, keyword substitution rules (exact/contains/regex).
- **History** — per-record storage with audio files, grouped by date, with playback and detail view.
- **Configurable retention policies** for text and audio.
- Vue 3 + TypeScript + Naive UI frontend.
- Tauri 2 (Rust) desktop shell.
