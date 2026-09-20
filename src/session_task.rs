//! Serialize preparation and destruction for one terminal without blocking GTK.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

#[derive(Default)]
pub struct SessionTask {
    closed: AtomicBool,
    operation: Mutex<()>,
}

impl SessionTask {
    pub fn prepare(&self, work: impl FnOnce()) -> bool {
        let _guard = self.operation.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_closed() {
            return false;
        }
        work();
        !self.is_closed()
    }

    /// Mark closed immediately on GTK, before queueing the destruction worker.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn finish_close(&self, work: impl FnOnce()) {
        let _guard = self.operation.lock().unwrap_or_else(|e| e.into_inner());
        work();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    #[test]
    fn queued_prepare_cannot_resurrect_a_closed_terminal() {
        let task = SessionTask::default();
        task.close();
        assert!(!task.prepare(|| panic!("closed terminal must not be prepared")));
    }

    #[test]
    fn destruction_waits_for_in_flight_preparation() {
        let task = Arc::new(SessionTask::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let prepare = Arc::clone(&task);
        let worker = std::thread::spawn(move || {
            prepare.prepare(|| {
                started_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            })
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        task.close(); // Must not wait for the worker.
        resume_tx.send(()).unwrap();
        task.finish_close(|| assert!(task.is_closed()));
        assert!(!worker.join().unwrap());
        assert!(!task.prepare(|| panic!("must stay closed")));
    }
}
