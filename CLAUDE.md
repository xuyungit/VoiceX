# Repository Guidelines

## Project Structure & Module Organization
- `src/` Vue 3 app: `components/` shared UI, `views/` route pages (Overview, History, Dictionary, settings, About), `stores/` Pinia state, `styles/` global tokens, `assets/` static files, `hud/` lightweight overlay assets.
- `public/` static assets copied as-is; `dist/` built output (do not edit).
- `src-tauri/` Rust shell for the desktop app: `src/` Tauri commands, `icons/` packaging assets, `tauri.conf.json` build/runtime config.
- `docs/` and `swift_ui_references/` hold reference material; keep generated artifacts out of these folders.

## Build, Test, and Development Commands
- Use `pnpm install` (preferred) for JS deps; ensure the Rust toolchain is installed for Tauri builds.
- `pnpm dev` runs the Vite dev server; `pnpm tauri dev` launches the desktop app (reuses the Vite server defined in `tauri.conf.json`).
- `pnpm build` type-checks via `vue-tsc --noEmit` then builds production assets into `dist/`.
- `pnpm preview` serves the built web bundle; `pnpm tauri build` packages the desktop app with the config in `src-tauri/tauri.conf.json`.

## Coding Style & Naming Conventions
- TypeScript with `<script setup>`; import Composition API symbols at the top.
- Two-space indentation, single quotes, and consistent trailing semicolon usage within each file.
- Components in `PascalCase` (`Sidebar.vue`), routes use `kebab-case` paths (`/llm-settings`), Pinia stores are `camelCase` (`useSettingsStore`), and enums/constants are `SCREAMING_SNAKE_CASE`.
- Prefer scoped styles in SFCs and reuse variables/mixins from `src/styles/main.css`; keep HUD styling self-contained in `src/hud/`.
- Resist the urge to implement 'defensive fallbacks' or quick-fix patches. Analyze the underlying cause. If a failure occurs, visibility is often more critical than concealment.
- When adding new features, consider cross-platform availability (macOS/Windows) instead of targeting only the current platform.

## Testing Guidelines
- No automated suite yet; when adding, colocate Vitest specs as `*.spec.ts` near the source and use `#[cfg(test)]` modules for Rust tests in `src-tauri/src`.
- Prioritize coverage for routing, store actions (settings/history persistence), and Tauri command boundaries; run `pnpm build` for a quick type-safety check until tests exist.

## Commit & Pull Request Guidelines
- Follow conventional commits (`feat: ...`, `fix: ...`, `chore: ...`); current history uses this style.
- PRs should include a brief summary, linked issue, screenshots/GIFs for UI changes, and notes on testing or risks (ASR/LLM/audio flows, settings persistence, packaging).
- Keep changes scoped and mention follow-ups if work is intentionally deferred.
