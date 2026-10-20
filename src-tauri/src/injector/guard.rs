//! Preconditions for sending input, without reading the destination's text.

use std::sync::Arc;
#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, Instant};

use super::InjectorError;

#[cfg(any(target_os = "macos", test))]
const POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(target_os = "macos")]
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(3);

/// A cancellation predicate belongs to one attempt. Session callers include
/// their generation in it, so resetting a shared cancel flag cannot revive an
/// older attempt that was waiting for the injection lock.
#[derive(Clone)]
pub struct InjectionGuard {
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Default for InjectionGuard {
    fn default() -> Self {
        Self::new(|| false)
    }
}

impl InjectionGuard {
    pub fn new(cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            cancelled: Arc::new(cancelled),
        }
    }

    pub(super) fn check_cancelled(&self) -> Result<(), InjectorError> {
        if (self.cancelled)() {
            return Err(InjectorError::Cancelled);
        }
        Ok(())
    }

    /// Check again at the actual event boundary; clearing an event's flags
    /// does not mean the user has released their physical shortcut chord.
    #[cfg(target_os = "macos")]
    pub(super) fn check_before_event(&self) -> Result<(), InjectorError> {
        self.check_cancelled()?;
        // The user chooses the destination by placing focus. A focus change
        // must not cancel input or bind it back to the recording's original app.
        if modifiers_held() {
            return Err(InjectorError::ModifiersHeld);
        }
        Ok(())
    }

    pub(super) fn wait_until_ready(&self) -> Result<(), InjectorError> {
        self.check_cancelled()?;
        #[cfg(target_os = "macos")]
        {
            self.wait_for_release(MODIFIER_RELEASE_TIMEOUT, modifiers_held)?;
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", test))]
    fn wait_for_release(
        &self,
        timeout: Duration,
        mut held: impl FnMut() -> bool,
    ) -> Result<(), InjectorError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.check_cancelled()?;
            let is_held = held();
            self.check_cancelled()?;
            if !is_held {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(InjectorError::ModifiersHeld);
            }
            self.sleep(POLL_INTERVAL)?;
        }
    }

    /// Keep waits cancellable, including while a clipboard bridge cycles focus.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn sleep(&self, duration: Duration) -> Result<(), InjectorError> {
        let deadline = Instant::now() + duration;
        loop {
            self.check_cancelled()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(POLL_INTERVAL));
        }
    }
}

#[cfg(target_os = "macos")]
fn modifiers_held() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceFlagsState(state_id: u32) -> u64;
    }
    // HIDSystemState is the physical state, independent of our posted events.
    const HID_SYSTEM_STATE: u32 = 1;
    // Command, Control, Option, Shift and Fn. Caps Lock is a toggle, not a chord.
    const MODIFIERS: u64 = 0x0010_0000 | 0x0004_0000 | 0x0008_0000 | 0x0002_0000 | 0x0080_0000;
    unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) & MODIFIERS != 0 }
}

/// Bound the lifetime of queued main-thread work. A rendezvous cannot leave
/// a successful activation buffered after its receiver has timed out; an
/// undeliverable result is undone on the same thread that performed the work.
#[cfg(any(target_os = "macos", test))]
pub(super) fn dispatch_and_wait<T: Send + 'static>(
    dispatch: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), ()>,
    work: impl FnOnce() -> T + Send + 'static,
    abandoned: impl FnOnce(T) + Send + 'static,
    timeout: Duration,
) -> Option<T> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    let queued = Arc::new(AtomicBool::new(true));
    let callback_queued = queued.clone();
    dispatch(Box::new(move || {
        if callback_queued.swap(false, Ordering::SeqCst) {
            if let Err(std::sync::mpsc::SendError(result)) = tx.send(work()) {
                abandoned(result);
            }
        }
    }))
    .ok()?;
    let result = rx.recv_timeout(timeout).ok();
    queued.store(false, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn resetting_the_shared_cancel_flag_does_not_revive_an_old_attempt() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(7));
        let guard = {
            let cancelled = cancelled.clone();
            let generation = generation.clone();
            InjectionGuard::new(move || {
                cancelled.load(Ordering::SeqCst) || generation.load(Ordering::SeqCst) != 7
            })
        };
        assert!(guard.check_cancelled().is_ok());
        cancelled.store(true, Ordering::SeqCst);
        generation.store(8, Ordering::SeqCst);
        cancelled.store(false, Ordering::SeqCst);
        assert!(matches!(
            guard.check_cancelled(),
            Err(InjectorError::Cancelled)
        ));
    }

    #[test]
    fn held_modifiers_timeout_without_sending_input() {
        let guard = InjectionGuard::default();
        assert!(matches!(
            guard.wait_for_release(Duration::ZERO, || true),
            Err(InjectorError::ModifiersHeld)
        ));
        guard.wait_for_release(Duration::ZERO, || false).unwrap();
    }

    #[test]
    fn cancellation_wins_even_when_modifiers_have_just_been_released() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let guard = InjectionGuard::new(move || flag.load(Ordering::SeqCst));
        assert!(matches!(
            guard.wait_for_release(Duration::ZERO, || {
                cancelled.store(true, Ordering::SeqCst);
                false
            }),
            Err(InjectorError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_interrupts_a_pre_paste_wait() {
        let guard = InjectionGuard::new(|| true);
        let started = Instant::now();
        assert!(matches!(
            guard.sleep(Duration::from_secs(10)),
            Err(InjectorError::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn expired_queued_focus_work_never_runs() {
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        assert_eq!(
            dispatch_and_wait(
                |task| {
                    tx.send(task).unwrap();
                    Ok(())
                },
                move || {
                    work_ran.store(true, Ordering::SeqCst);
                },
                |_| panic!("unstarted work needs no cleanup"),
                Duration::ZERO,
            ),
            None
        );
        rx.recv().unwrap()();
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn abandoned_running_focus_work_cleans_up_on_its_dispatch_thread() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cleaned_tx, cleaned_rx) = std::sync::mpsc::channel();
        let result = dispatch_and_wait(
            |task| {
                std::thread::spawn(task);
                // The work has already started before the receive timeout.
                started_rx.recv().unwrap();
                Ok(())
            },
            move || {
                let thread = std::thread::current().id();
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                (42, thread)
            },
            move |(value, thread)| {
                assert_eq!(thread, std::thread::current().id());
                cleaned_tx.send(value).unwrap();
            },
            Duration::ZERO,
        );
        assert!(result.is_none());
        release_tx.send(()).unwrap();
        assert_eq!(cleaned_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 42);
    }

    #[test]
    fn completed_focus_work_delivers_its_result_without_cleanup() {
        assert_eq!(
            dispatch_and_wait(
                |task| {
                    std::thread::spawn(task);
                    Ok(())
                },
                || 42,
                |_| panic!("delivered work must remain in effect"),
                Duration::from_secs(1),
            ),
            Some(42)
        );
    }
}
