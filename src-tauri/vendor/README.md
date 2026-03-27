# Vendored Dependencies

This directory contains forked or pinned copies of upstream crates that cannot be used as-is from crates.io. Each is patched via `[patch.crates-io]` in the workspace `Cargo.toml`.

## audiopus_sys

Upstream crate has cross-platform build reliability issues — `.cargo-ok` marker files and CMake path resolution can break when building for multiple targets (Windows / macOS / Linux) in a monorepo. Vendoring the crate with its bundled Opus 1.3 source ensures:

- Consistent static linking without requiring a system-installed `libopus`.
- Predictable CMake invocation (paired with the root-level `.cargo/config.toml` + `cmake-wrap.cmd` for Windows CMake policy compatibility).

No functional code changes to the upstream crate; the vendoring itself is the fix.

## rdev

Upstream rdev (0.5) uses `static mut` for global event callbacks, which is unsound in multi-threaded async contexts like Tauri's tokio runtime. Key modifications:

- **Thread-safe callbacks**: `GLOBAL_CALLBACK` wrapped in `lazy_static! + Mutex<Option<…>>` on both macOS and Windows.
- **`Send` bounds**: Added to `listen()` / `grab()` callback closures so they can be safely moved across threads.
- **Windows raw pointer fix**: Switched to `&raw mut` syntax for hook callback dereferencing.
- **macOS keyboard state**: Removed shared `KEYBOARD_STATE` lazy_static to eliminate lock contention during event processing.

These changes are necessary for safe global hotkey capture in an async desktop application.
