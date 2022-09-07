use std::{
    sync::{
        Mutex,
        Arc,
        Condvar
    },
    time::Duration
};

use wasmer_wasi_types::__wasi_signal_t;

/// Represents a running thread which allows a joiner to
/// wait for the thread to exit
#[derive(Debug, Clone)]
pub struct WasiThread
{
    finished: Arc<(Mutex<bool>, Condvar)>,
    signals: Arc<Mutex<Vec<__wasi_signal_t>>>,
}

impl Default
for WasiThread
{
    fn default() -> Self
    {
        Self {
            finished: Arc::new((Mutex::new(false), Condvar::default())),
            signals: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl WasiThread
{
    /// Marks the thread as finished (which will cause anyone that
    /// joined on it to wake up)
    pub fn mark_finished(&self) {
        let mut guard = self.finished.0.lock().unwrap();
        *guard = true;
        self.finished.1.notify_all();
    }

    /// Waits until the thread is finished or the timeout is reached
    pub fn join(&self, timeout: Duration) -> bool {
        let mut finished = self.finished.0.lock().unwrap();
        if *finished == true {
            return true;
        }
        loop {
            let woken = self.finished.1.wait_timeout(finished, timeout).unwrap();
            if woken.1.timed_out() {
                return false;
            }
            finished = woken.0;
            if *finished == true {
                return true;
            }
        }
    }

    /// Adds a signal for this thread to process
    pub fn signal(&self, signal: __wasi_signal_t) {
        let mut guard = self.signals.lock().unwrap();
        if guard.contains(&signal) == false {
            guard.push(signal);
        }
    }

    /// Returns all the signals that are waiting to be processed
    pub fn pop_signals(&self) -> Vec<__wasi_signal_t> {
        let mut guard = self.signals.lock().unwrap();
        guard.drain(..).collect()
    }
}
