use tauri::async_runtime;

use crate::injector::{
    inject_serialized, inject_serialized_with_resolver, InjectionGuard, TextInjectionMode,
    TextInjector,
};

/// Handles text injection off the main async tasks to avoid blocking.
#[derive(Clone, Default)]
pub struct TextInjectionService;

impl TextInjectionService {
    pub fn new() -> Self {
        Self
    }

    /// Fire-and-forget text injection using the configured mode.
    ///
    /// `skip_clipboard_restore` should be true when the target's per-app
    /// override has `skip_clipboard_restore` set, since those targets bridge
    /// the clipboard over a variable-latency channel where a timed restore
    /// is not safe to perform.
    ///
    /// `focus_roundtrip_pid` optionally identifies the paste target's
    /// process for the pre-paste focus round-trip (macOS only).
    pub fn inject_background(
        &self,
        mode: TextInjectionMode,
        text: String,
        skip_clipboard_restore: bool,
        focus_roundtrip_pid: Option<u32>,
    ) {
        if text.is_empty() {
            return;
        }

        async_runtime::spawn_blocking(move || {
            if let Err(err) =
                inject_serialized(mode, &text, skip_clipboard_restore, focus_roundtrip_pid)
            {
                log::warn!("Text injection failed: {}", err);
            }
        });
    }

    /// Inject text in a blocking thread with a cancellation guard.
    /// Returns a JoinHandle so callers can await completion.
    ///
    /// Resolve the current destination inside the injection lock, after
    /// waiting for physical modifiers, instead of using the recording's app.
    pub fn inject_background_guarded(
        &self,
        text: String,
        guard: InjectionGuard,
        resolve: impl FnOnce() -> TextInjector + Send + 'static,
    ) -> Option<tauri::async_runtime::JoinHandle<()>> {
        if text.is_empty() {
            return None;
        }

        Some(async_runtime::spawn_blocking(move || {
            if let Err(err) = inject_serialized_with_resolver(&text, guard, resolve) {
                log::warn!("Text injection failed: {}", err);
            }
        }))
    }
}
