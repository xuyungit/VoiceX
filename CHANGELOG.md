# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.10.0] - 2026-04-26

### Added
- **StepAudio 2.5 ASR** — added StepFun batch transcription via HTTP + SSE, including API key/model settings, provider selection, history metadata, and re-transcription support.
- **Per-app text injection overrides** — Input Settings can now remember recent target apps and choose pasteboard or typing mode per application, so apps with special editor behavior can use their own injection strategy.
- **LLM connectivity test** — Settings → LLM can send a real correction probe with the active provider and model, then show status, response time, test input, and model output.
- **History replay injection test** — saved recordings can now be re-transcribed, post-processed, and injected into the current foreground app for end-to-end provider and injection checks.

### Changed
- Expanded VoiceX from eleven to twelve ASR backends.
- Re-transcription now returns the final post-processed text in addition to ASR and LLM intermediate results, and it respects the original history mode when choosing assistant vs. translation prompts.
- Clipboard injection and macOS paste shortcuts now use steadier timing to improve reliability in editors that process paste events slowly.
- The compact HUD now keeps the processing intent chip visible during batch and compact states.

### Fixed
- Prevented the HUD from stealing input focus and preserved the original foreground app context when recording starts.
- Hardened hotkey session races around rapid start/stop/cancel flows.
- Empty Qwen sessions that exit because of silence are now handled without leaving stale session state.

## [0.9.5] - 2026-04-09

### Changed
- Refined the Fun-ASR settings UI so placeholder text follows the active locale and failure states reuse shared theme tokens.
- Short accidental recordings under 800 ms are now discarded before transcription starts.

### Fixed
- On macOS, the HUD overlay now follows the currently active Space when recording is triggered from another desktop.
- Very quiet dictation is no longer prefiltered as "silent" before batch recognition, so low-volume recordings can still reach the ASR pipeline.

## [0.9.4] - 2026-04-08

### Added
- **Fun-ASR realtime provider** — added DashScope Fun-ASR as a dedicated low-latency realtime backend, including region-specific endpoints, model selection, and optional language hints.

### Changed
- Expanded VoiceX from ten to eleven ASR backends.
- Updated Qwen batch handling to prefer compressed recording files and to surface provider-imposed recording caps directly in Settings when users configure a longer duration.

### Fixed
- Failed batch transcriptions are now preserved in local history with their audio and error details intact, so users can retry re-transcription later instead of losing the recording immediately.
- Qwen batch and Qwen realtime-plus-batch-refine now stop at the provider's current five-minute hard limit instead of recording longer and failing only at upload time.
- Qwen batch size preflight now checks the real Base64 request payload size, reducing avoidable `input_audio.data` oversize failures.

## [0.9.3] - 2026-04-06

### Added
- **Configurable Soniox endpoint delay** — the Soniox settings page now exposes `max_endpoint_delay_ms`, and leaving it empty preserves the provider default instead of forcing a hardcoded delay.

### Changed
- Refined the built-in Assistant correction prompt so default cleanup handles self-corrections, light transcript cleanup, and spoken-number normalization more reliably without over-formatting casual input.
- Refined the built-in Translation prompt so default translation better strips filler speech, corrects obvious ASR errors conservatively, and preserves technical terms before translating.

## [0.9.2] - 2026-04-04

### Added
- **ElevenLabs Speech-to-Text** — new ASR provider with realtime streaming, batch transcription, and optional post-recording batch refine flow.
- **ElevenLabs re-transcription and provider probe support** — saved recordings and provider checks can now exercise the ElevenLabs pipeline directly.

### Changed
- Expanded VoiceX from nine to ten ASR backends.
- Updated the settings UI and provider documentation to reflect ElevenLabs mode selection, model selection, language hints, and keyterm support.

### Fixed
- Improved failure handling for ElevenLabs batch and refine flows so errors stay visible and do not silently collapse the recording lifecycle.
- Disabling diagnostics now clears Soniox debug overrides immediately, preventing stale fault injection or mock-server state from leaking into later sessions.

## [0.9.1] - 2026-03-31

### Added
- **Real RMS-driven batch HUD waveform** — batch recording HUD now reacts to live input level instead of using a fake placeholder animation.
- **Compact batch HUD presentation** — batch mode now uses a narrower waveform-focused layout instead of reusing the wide streaming text window.
- **macOS text injection notes** — documented the newline-related typing-mode limitations and the current fallback strategy.

### Changed
- HUD presentation is now explicitly separated between `stream` and `batch` modes so batch processing can stay in a compact status-only flow.
- macOS multiline typing injection now falls back to pasteboard mode to avoid newline-triggered IME and text ordering issues.

### Fixed
- Fixed intermittent HUD flicker where a larger frame could flash briefly during HUD reuse and batch stage transitions on high-DPI displays.
- Fixed batch HUD stage transitions so batch recognition/correction no longer bounce back through the streaming text presentation.
- Improved macOS typing-mode reliability for multiline text injection.

## [0.9.0] - 2026-03-29

### Added
- **Soniox real-time ASR** — cloud streaming provider via WebSocket (`stt-rt-v4` model) with hotword support and language hints.
- **OpenAI ASR** — dual-mode provider supporting both batch file upload (`gpt-4o-transcribe`) and WebSocket realtime streaming, with language detection and prompt-based hotword injection.
- **Redesigned Overview page** — new status bar and reorganized stat cards for clearer at-a-glance metrics.
- **Tag-based hotword editor** — chip UI replaces the plain textarea for managing the hotword list.

### Changed
- Expanded VoiceX from seven to nine ASR backends.
- Sidebar navigation reorganized with grouped sections; "Dictionary" renamed to "Hotwords" across the UI.
- HUD text truncation now uses pixel-based measurement instead of character count, improving display accuracy for CJK text.
- ASR settings refactored into per-provider components for cleaner configuration.

### Fixed
- Soniox: trailing non-final tokens are now preserved at session end; correct model name shown in history records.
- Statistics: robust backfill for `total_recording_count` across per-device and sync scenarios.
- Windows CI: fixed pnpm workspace declaration, cache handling, and release workflow.

## [0.8.0] - 2026-03-27

### Added
- **Gemini Audio Transcription** batch ASR provider for whole-file uploads after recording stops.
- **Gemini Live Realtime** ASR provider with live input-audio transcription and configurable language hints.
- **Cohere Audio Transcription** batch ASR provider with configurable model and language code.
- **Provider comparison via re-transcription** — saved recordings can now be re-run through Gemini, Gemini Live, and Cohere in addition to the existing providers.
- **Automated Windows release packaging** via GitHub Actions, so publishing a GitHub Release can attach Windows installers without a separate Windows development machine.

### Changed
- Expanded VoiceX from four to seven ASR backends, covering both realtime streaming and higher-quality batch transcription workflows.
- Refreshed the English and Chinese README files to document the new ASR options, bilingual interface coverage, re-transcription workflow, and release process.

### Fixed
- Gemini Live is now exposed correctly in the re-transcription dialog so it can be selected for history-based comparisons.

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
