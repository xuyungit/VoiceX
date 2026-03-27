use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use tokio::time::sleep;

/// Simple countdown service with start/cancel using tokio tasks.
#[derive(Clone, Default)]
pub struct CountdownService {
    handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl CountdownService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a countdown (seconds). Calls `on_tick` for remaining seconds and `on_complete` at end.
    /// Cancels any existing countdown.
    pub fn start<F, C>(&self, total_secs: u64, mut on_tick: F, mut on_complete: C)
    where
        F: FnMut(u32) + Send + 'static,
        C: FnMut() + Send + 'static,
    {
        self.cancel();

        let handle = tauri::async_runtime::spawn(async move {
            for remaining in (0..=total_secs).rev() {
                on_tick(remaining as u32);

                if remaining == 0 {
                    break;
                }
                sleep(Duration::from_secs(1)).await;
            }

            on_complete();
        });

        if let Ok(mut guard) = self.handle.lock() {
            *guard = Some(handle);
        }
    }

    /// Cancel any active countdown. Returns `true` if a countdown was actually cancelled.
    pub fn cancel(&self) -> bool {
        if let Ok(mut guard) = self.handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                return true;
            }
        }
        false
    }
}
