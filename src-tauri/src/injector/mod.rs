//! Text injection module

mod clipboard;

pub use clipboard::{InjectorError, TextInjectionMode, TextInjector};

use std::sync::Mutex;

/// Global mutex to prevent concurrent text injections.
/// Two simultaneous clipboard pastes or typing sequences would corrupt output.
static INJECTION_MUTEX: Mutex<()> = Mutex::new(());

/// Acquire the global injection lock, inject text, then release.
/// This guarantees at most one injection runs at a time.
pub fn inject_serialized(mode: TextInjectionMode, text: &str) -> Result<(), InjectorError> {
    let _guard = INJECTION_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    TextInjector::with_mode(mode).inject(text)
}
