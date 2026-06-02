//! Terminal event polling.
//!
//! A single blocking poll yields either a key press or a periodic tick, so the
//! draw loop stays simple and synchronous. Non-key terminal events (resize,
//! mouse, paste) collapse into a [`AppEvent::Tick`] that triggers a redraw.

use std::io;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};

/// What the loop reacts to.
pub enum AppEvent {
    Key(KeyEvent),
    Tick,
}

/// Drain a channel non-blockingly, returning only the most recent value
/// (coalescing any backlog). `None` if nothing is pending.
pub fn drain_latest<T>(rx: &Receiver<T>) -> Option<T> {
    let mut latest = None;
    while let Ok(value) = rx.try_recv() {
        latest = Some(value);
    }
    latest
}

/// Wait up to `timeout` for a terminal event.
///
/// # Errors
/// Propagates IO errors from the underlying crossterm reader.
pub fn poll(timeout: Duration) -> io::Result<AppEvent> {
    if event::poll(timeout)? {
        if let Event::Key(key) = event::read()? {
            // Ignore key-release/repeat on platforms that report them.
            if key.kind == KeyEventKind::Press {
                return Ok(AppEvent::Key(key));
            }
        }
    }
    Ok(AppEvent::Tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn drain_latest_coalesces_to_newest() {
        let (tx, rx) = mpsc::channel();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        tx.send(3).unwrap();
        assert_eq!(drain_latest(&rx), Some(3));
        assert_eq!(drain_latest::<i32>(&rx), None);
    }
}
