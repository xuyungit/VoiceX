//! Serialize update installation with admission of desktop work.
//! Downloads do not take the exclusive permit, so dictation remains usable.

use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct WorkGate {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    workers: usize,
    installing: bool,
}

pub(crate) struct WorkPermit(WorkGate);
pub(crate) struct InstallPermit(WorkGate);

impl WorkGate {
    pub(crate) fn try_work(&self) -> Option<WorkPermit> {
        let mut state = self.inner.lock().unwrap();
        if state.installing {
            return None;
        }
        state.workers += 1;
        Some(WorkPermit(self.clone()))
    }

    pub(crate) fn try_install(&self) -> Option<InstallPermit> {
        let mut state = self.inner.lock().unwrap();
        if state.installing || state.workers != 0 {
            return None;
        }
        state.installing = true;
        Some(InstallPermit(self.clone()))
    }
}

impl Drop for WorkPermit {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().workers -= 1;
    }
}

impl Drop for InstallPermit {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().installing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_waits_for_every_worker_and_reopens_after_failure() {
        let gate = WorkGate::default();
        let session = gate.try_work().unwrap();
        let history_write = gate.try_work().unwrap();
        assert!(gate.try_install().is_none());
        drop(session);
        assert!(gate.try_install().is_none());
        drop(history_write);
        let install = gate.try_install().unwrap();
        assert!(gate.try_work().is_none());
        assert!(gate.try_install().is_none());
        drop(install);
        assert!(gate.try_work().is_some());
    }

    #[test]
    fn simultaneous_admission_and_installation_are_exclusive() {
        let gate = WorkGate::default();
        std::thread::scope(|scope| {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let start = barrier.clone();
            let gate = &gate;
            let worker = scope.spawn(move || {
                let permit = gate.try_work();
                start.wait();
                start.wait();
                permit
            });
            let install = gate.try_install();
            barrier.wait();
            // Both permits stay alive until after the two attempts.
            barrier.wait();
            let work = worker.join().unwrap();
            assert!(install.is_some() ^ work.is_some());
        });
    }
}
