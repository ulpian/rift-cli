//! Safe PTY driver with a real VT parser, bounded output and cleanup on every path.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};

use super::{ensure, fixture::Fixture, Result};

pub(super) struct Terminal {
    child: Box<dyn Child + Send + Sync>,
    // Keep the master alive until the child is reaped.
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    reader: Receiver<Vec<u8>>,
    parser: vt100::Parser,
    pub(super) raw: Vec<u8>,
}

impl Terminal {
    pub(super) fn new(fixture: &Fixture, command: &[&str], cwd: &Path) -> Result<Self> {
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut builder = CommandBuilder::new(command.first().ok_or("empty PTY command")?);
        builder.args(command.iter().skip(1));
        builder.cwd(cwd);
        builder.env_clear();
        for (key, value) in &fixture.env {
            builder.env(key, value);
        }
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let (send, receive) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            while let Ok(size) = reader.read(&mut buffer) {
                if size == 0 {
                    break;
                }
                if let Some(bytes) = buffer.get(..size) {
                    if send.send(bytes.to_vec()).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            _master: pair.master,
            writer,
            reader: receive,
            parser: vt100::Parser::new(32, 100, 0),
            raw: Vec::new(),
        })
    }

    pub(super) fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|e| e.to_string())
    }

    pub(super) fn capture(&mut self, duration: Duration) -> Result<String> {
        let deadline = Instant::now() + duration;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self
                .reader
                .recv_timeout(left.min(Duration::from_millis(100)))
            {
                Ok(bytes) => {
                    ensure(
                        self.raw.len() + bytes.len() <= 4_000_000,
                        "unexpected PTY output volume",
                    )?;
                    self.parser.process(&bytes);
                    self.raw.extend(bytes);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(self.parser.screen().contents())
    }

    pub(super) fn wait(&mut self, label: &str, seconds: u64) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let text = self.capture(Duration::from_millis(200))?;
            if text.contains(label) {
                return Ok(text);
            }
            if Instant::now() >= deadline
                || self.child.try_wait().map_err(|e| e.to_string())?.is_some()
            {
                return Err(format!("missing {label:?}:\n{text}"));
            }
        }
    }

    pub(super) fn click(&mut self, label: &str) -> Result<()> {
        let screen = self.capture(Duration::from_millis(200))?;
        let (row, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(label))
            .ok_or_else(|| format!("missing click target {label}:\n{screen}"))?;
        let column = line
            .find(label)
            .map(|index| line.get(..index).unwrap_or_default().chars().count())
            .ok_or("missing label")?;
        self.send(
            format!(
                "\x1b[<0;{};{}M\x1b[<0;{};{}m",
                column + 1,
                row + 1,
                column + 1,
                row + 1
            )
            .as_bytes(),
        )?;
        self.capture(Duration::from_millis(200))?;
        Ok(())
    }

    pub(super) fn advance(&mut self, title: &str) -> Result<String> {
        self.send("\x1b[B".repeat(30).as_bytes())?;
        self.capture(Duration::from_millis(200))?;
        self.send(b"\r")?;
        self.wait(title, 15)
    }

    pub(super) fn checked(&mut self, label: &str) -> Result<bool> {
        let text = self.capture(Duration::from_millis(200))?;
        let line = text
            .lines()
            .find(|line| line.contains(label) && line.contains(['●', '○']))
            .ok_or_else(|| format!("missing radio {label}:\n{text}"))?;
        Ok(line.contains('●'))
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.capture(Duration::from_millis(100))?;
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                return ensure(
                    status.success(),
                    &format!(
                        "terminal command failed:\n{}",
                        self.parser.screen().contents()
                    ),
                );
            }
            ensure(Instant::now() < deadline, "terminal command did not exit")?;
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.send(b"\x03");
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let _ = self.capture(Duration::from_millis(100));
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
