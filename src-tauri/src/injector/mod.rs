//! Text injection module

mod clipboard;
mod guard;
mod receipt;

pub use clipboard::{InjectorError, TextInjectionMode, TextInjector};
pub use guard::InjectionGuard;
use receipt::DispatchTracker;
pub use receipt::{AttemptIdentity, DispatchOutcome, InjectionReceipt, ResolvedApp};

use std::sync::Mutex;

/// App handle used to dispatch AppKit work that must run on the main thread
/// (the pasteboard focus round-trip's activation dance). Registered once
/// during app setup; macOS only.
#[cfg(target_os = "macos")]
pub(crate) static MAIN_THREAD_APP: std::sync::OnceLock<tauri::AppHandle> =
    std::sync::OnceLock::new();

/// Global mutex to prevent concurrent text injections.
/// Two simultaneous clipboard pastes or typing sequences would corrupt output.
static INJECTION_MUTEX: Mutex<()> = Mutex::new(());

/// Acquire the global injection lock, inject text, then release.
/// This guarantees at most one injection runs at a time.
///
/// `skip_clipboard_restore` should be true when the target's per-app override
/// has `skip_clipboard_restore` set (see
/// `foreground_app::match_text_injection_override`), i.e. it bridges the
/// clipboard over a variable-latency channel (e.g. a remote-desktop client)
/// where a timed restore is not safe to perform.
///
/// `focus_roundtrip_pid` optionally identifies the paste target's process.
/// When set, the pasteboard path first bounces focus through VoiceX so the
/// target's clipboard bridge announces the freshly written payload before
/// the paste is synthesized — remote-desktop clients only re-read the Mac
/// clipboard when they become active again (macOS only; ignored elsewhere).
pub fn inject_serialized(
    mode: TextInjectionMode,
    text: &str,
    skip_clipboard_restore: bool,
    focus_roundtrip_pid: Option<u32>,
) -> Result<(), InjectorError> {
    inject_serialized_with_guard(
        mode,
        text,
        skip_clipboard_restore,
        focus_roundtrip_pid,
        InjectionGuard::default(),
    )
}

pub fn inject_serialized_with_guard(
    mode: TextInjectionMode,
    text: &str,
    skip_clipboard_restore: bool,
    focus_roundtrip_pid: Option<u32>,
    guard: InjectionGuard,
) -> Result<(), InjectorError> {
    inject_serialized_with_resolver(text, guard, || {
        TextInjector::with_mode(mode, skip_clipboard_restore)
            .with_focus_roundtrip_target(focus_roundtrip_pid)
    })
}

/// Resolve app-specific settings after queued work and physical modifiers have
/// cleared, so an app selected during either wait is the actual destination.
pub fn inject_serialized_with_resolver(
    text: &str,
    guard: InjectionGuard,
    resolve: impl FnOnce() -> TextInjector,
) -> Result<(), InjectorError> {
    with_injection_lock(&INJECTION_MUTEX, &guard, || {
        if text.is_empty() {
            return Ok(());
        }
        guard.wait_until_ready()?;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        crate::post_edit::coordinator::dispatch_boundary();
        let injector = resolve().with_guard(guard.clone());
        // Foreground-app lookup may wait on the main thread. Cancellation
        // during that lookup must also prevent native Windows input.
        guard.check_cancelled()?;
        injector.inject_ready(text)
    })
}

/// Report system input dispatch independently from whether the target accepted it.
pub fn inject_serialized_with_receipt(
    text: &str,
    guard: InjectionGuard,
    identity: AttemptIdentity,
    resolve: impl FnOnce() -> (TextInjector, Option<ResolvedApp>),
) -> InjectionReceipt {
    let tracker = DispatchTracker::default();
    let result = with_injection_lock(&INJECTION_MUTEX, &guard, || {
        if text.is_empty() {
            return Err(InjectorError::EmptyText);
        }
        guard.wait_until_ready()?;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        crate::post_edit::coordinator::dispatch_boundary();
        let (injector, resolved_app) = resolve();
        tracker.resolved_app(resolved_app);
        let injector = injector
            .with_guard(guard.clone())
            .with_dispatch_tracker(tracker.clone());
        guard.check_cancelled()?;
        injector.inject_ready(text)
    });
    if let Err(ref error) = result {
        log::warn!("Text injection failed: {}", error);
    }
    tracker.finish(identity, result)
}

fn with_injection_lock<T>(
    mutex: &Mutex<()>,
    guard: &InjectionGuard,
    inject: impl FnOnce() -> Result<T, InjectorError>,
) -> Result<T, InjectorError> {
    let _lock = mutex.lock().unwrap_or_else(|e| e.into_inner());
    // A pre-queue check is insufficient: cancellation may happen while a
    // previous injection or delayed clipboard restore holds the mutex.
    guard.check_cancelled()?;
    inject()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn a_cancelled_waiter_never_enters_the_injection() {
        let mutex = Arc::new(Mutex::new(()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Barrier::new(2));
        let lock = mutex.lock().unwrap();
        let worker = {
            let mutex = mutex.clone();
            let cancelled = cancelled.clone();
            let entered = entered.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                let guard = InjectionGuard::new(move || cancelled.load(Ordering::SeqCst));
                ready.wait();
                with_injection_lock(&mutex, &guard, || {
                    entered.store(true, Ordering::SeqCst);
                    Ok(())
                })
            })
        };
        ready.wait();
        cancelled.store(true, Ordering::SeqCst);
        drop(lock);
        assert!(matches!(
            worker.join().unwrap(),
            Err(InjectorError::Cancelled)
        ));
        assert!(!entered.load(Ordering::SeqCst));
    }

    #[test]
    fn cancelled_receipt_does_not_resolve_or_dispatch_a_target() {
        let receipt = inject_serialized_with_receipt(
            "example",
            InjectionGuard::new(|| true),
            AttemptIdentity { session_id: uuid::Uuid::new_v4(), record_id: uuid::Uuid::new_v4(), injection_epoch: 12 },
            || panic!("cancelled input must not resolve a target"),
        );
        assert_eq!(receipt.outcome, DispatchOutcome::CancelledBeforeDispatch);
        assert_eq!(receipt.injection_epoch, 12);
        assert!(receipt.dispatch_started_at.is_none());
        assert!(receipt.dispatch_finished_at.is_none());
    }
}
