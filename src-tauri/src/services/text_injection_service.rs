use tauri::async_runtime;

use crate::injector::{TextInjectionMode, TextInjector};

/// Handles text injection off the main async tasks to avoid blocking.
#[derive(Clone, Default)]
pub struct TextInjectionService;

impl TextInjectionService {
    pub fn new() -> Self {
        Self
    }

    /// Fire-and-forget text injection using the configured mode.
    pub fn inject_background(&self, mode: TextInjectionMode, text: String) {
        if text.is_empty() {
            return;
        }

        async_runtime::spawn_blocking(move || {
            let injector = TextInjector::with_mode(mode);
            if let Err(err) = injector.inject(&text) {
                log::warn!("Text injection failed: {}", err);
            }
        });
    }

    /// Fire-and-forget text injection with a guard flag; returns the guard so callers can keep it alive.
    pub fn inject_background_guarded(
        &self,
        mode: TextInjectionMode,
        text: String,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        if text.is_empty() {
            return;
        }

        async_runtime::spawn_blocking(move || {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("Injection skipped: cancelled before start");
                return;
            }
            let injector = TextInjector::with_mode(mode);
            if let Err(err) = injector.inject(&text) {
                log::warn!("Text injection failed: {}", err);
            }
        });
    }
}
