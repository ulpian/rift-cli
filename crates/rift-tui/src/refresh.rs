//! Background metrics refresh.
//!
//! Reading the `entire/checkpoints/v1` branch shells out to `git`, which can be
//! slow, so it must never run on the UI thread. [`MetricsRefresher`] owns a
//! worker thread that reloads the snapshot on an interval and hands the newest
//! one to the loop via [`MetricsRefresher::latest`]. Dropping it stops and joins
//! the worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::event::drain_latest;
use crate::metrics::MetricsSnapshot;

/// A handle to the background metrics-refresh thread.
pub struct MetricsRefresher {
    rx: Receiver<MetricsSnapshot>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MetricsRefresher {
    /// Spawn a worker that loads metrics for `repo` immediately, then every
    /// `interval`.
    #[must_use]
    pub fn spawn(repo: PathBuf, interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);

        let handle = thread::spawn(move || loop {
            // The receiver is gone once the UI loop exits; stop sending.
            if tx.send(MetricsSnapshot::load(&repo)).is_err() {
                return;
            }
            // Sleep `interval` in slices so `stop` is honored promptly.
            let slice = Duration::from_millis(100);
            let mut waited = Duration::ZERO;
            while waited < interval {
                if worker_stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(slice);
                waited += slice;
            }
        });

        Self {
            rx,
            stop,
            handle: Some(handle),
        }
    }

    /// The most recent snapshot (coalescing any backlog), or `None` if no new
    /// snapshot has arrived since the last call.
    #[must_use]
    pub fn latest(&self) -> Option<MetricsSnapshot> {
        drain_latest(&self.rx)
    }
}

impl Drop for MetricsRefresher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_an_initial_snapshot_and_stops_cleanly() {
        // A path with no git repo yields an empty snapshot quickly.
        let refresher = MetricsRefresher::spawn(PathBuf::from("."), Duration::from_secs(60));

        // The worker sends its first snapshot promptly; poll briefly for it.
        let mut got = None;
        for _ in 0..50 {
            if let Some(snapshot) = refresher.latest() {
                got = Some(snapshot);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(got.is_some(), "expected an initial metrics snapshot");
        // Dropping `refresher` here must stop+join the worker without hanging.
    }
}
