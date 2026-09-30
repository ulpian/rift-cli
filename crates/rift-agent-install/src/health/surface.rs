//! Which surface an agent runs on, so a remedy can say where to act.
//!
//! A separate module because it is the one place that decides where the
//! user is: the session record the agent wrote first, the presence evidence
//! second. Claude Code stamps an `entrypoint` on its transcript records and
//! Codex an `originator` on its session file's first line; a session cache
//! Rift already keeps names that file, and nothing else is opened. When no
//! record answers, [`crate::detect::installations`] does, so presence stays
//! the one predicate and no directory of any agent is walked here.
//!
//! Both readers share one bounded head reader: a fixed number of whole lines
//! within one mebibyte, so a transcript of any size answers in bounded time,
//! and an unreadable or torn head is no answer rather than an error.

use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

use rift_shadow_hooks::session_cache::SessionCache;
use serde::{Deserialize, Serialize};

use crate::detect::{
    self, DetectionEnvironment, NativeIdeHost, NativeInstallSource, NativeInstallation,
};
use crate::native::{codex_client::codex_client_at, CodexSurface};
use crate::ClientFamily;

/// The client an agent's user acts in.
///
/// Serialised as one flat string per surface (`vscode`, `terminal`, …)
/// rather than serde's tagged form of [`Surface::Ide`], because the string is
/// a `--json` contract a script matches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "&'static str", try_from = "String")]
pub enum Surface {
    /// A VS Code-compatible editor host.
    Ide(NativeIdeHost),
    /// A shell: the command-line interface, or a process started from one.
    Terminal,
    /// A JetBrains IDE's plugin.
    JetBrains,
    /// No record answered and the presence evidence names no single surface.
    Unknown,
}

/// Where a [`Surface`] answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceEvidence {
    /// The session record the agent itself wrote.
    SessionRecord,
    /// What is installed on this machine.
    Presence,
}

/// The one field of a Claude Code transcript record this module reads; every
/// other field is skipped unparsed.
#[derive(Deserialize)]
struct EntrypointRecord {
    entrypoint: Option<String>,
}

/// How many bytes of a session file's head are read, at most. Codex's first
/// line carries the session's base instructions and runs to tens of
/// kilobytes; a head longer than this is not read past it.
const HEAD_MAX_BYTES: u64 = 1024 * 1024;

/// How many lines of a Claude Code transcript are searched for an
/// `entrypoint`. The records that open a transcript carry none; the first
/// that does comes within a handful of lines.
const CLAUDE_HEAD_LINES: usize = 64;

impl Surface {
    /// The stable string, the same one serde writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ide(NativeIdeHost::VsCode) => "vscode",
            Self::Ide(NativeIdeHost::VsCodeInsiders) => "vscode_insiders",
            Self::Ide(NativeIdeHost::Cursor) => "cursor",
            Self::Ide(NativeIdeHost::Windsurf) => "windsurf",
            Self::Terminal => "terminal",
            Self::JetBrains => "jetbrains",
            Self::Unknown => "unknown",
        }
    }
}

impl From<Surface> for &'static str {
    fn from(surface: Surface) -> Self {
        surface.as_str()
    }
}

impl TryFrom<String> for Surface {
    type Error = String;

    fn try_from(spelling: String) -> Result<Self, Self::Error> {
        let surface = match spelling.as_str() {
            "vscode" => Self::Ide(NativeIdeHost::VsCode),
            "vscode_insiders" => Self::Ide(NativeIdeHost::VsCodeInsiders),
            "cursor" => Self::Ide(NativeIdeHost::Cursor),
            "windsurf" => Self::Ide(NativeIdeHost::Windsurf),
            "terminal" => Self::Terminal,
            "jetbrains" => Self::JetBrains,
            "unknown" => Self::Unknown,
            _ => return Err(format!("unknown surface {spelling:?}")),
        };
        Ok(surface)
    }
}

/// The surface the agent with registry token `agent_token` runs on, and the
/// evidence that says so.
///
/// The newest session of that agent whose cache names a session file is
/// read first; only Claude Code and Codex write a record that names their
/// client. Every other agent, and either of those two when its record gives
/// no answer, is read from [`detect::installations`] under `env`.
#[must_use]
pub fn client_surface(
    agent_token: &str,
    sessions: &[SessionCache],
    env: &DetectionEnvironment,
) -> (Surface, SurfaceEvidence) {
    if let Some(surface) = recorded_surface(agent_token, sessions) {
        return (surface, SurfaceEvidence::SessionRecord);
    }
    let installations = detect::installations(agent_token, env);
    (presence_surface(&installations), SurfaceEvidence::Presence)
}

/// The session file the newest of `sessions` run by `agent_token` names, with
/// that session's update time: `(updated_at_ms, file)`. A session whose cache
/// names no file is passed over, not taken as the newest.
pub(crate) fn newest_transcript(
    sessions: &[SessionCache],
    agent_token: &str,
) -> Option<(i64, PathBuf)> {
    sessions
        .iter()
        .filter(|cache| cache.agent == agent_token)
        .filter_map(|cache| {
            let transcript = cache.transcript_path.as_deref()?;
            Some((cache.updated_at_ms, PathBuf::from(transcript)))
        })
        .max_by_key(|(updated, _)| *updated)
}

/// The first answer `parse` gives over the first `max_lines` whole lines of
/// the file at `path`, reading at most [`HEAD_MAX_BYTES`].
///
/// One error rule for every reader: a file that cannot be opened, a line
/// that is not UTF-8, and a line cut by the end of the file or by the byte
/// bound all end the search with no answer.
pub(crate) fn first_in_head<T>(
    path: &Path,
    max_lines: usize,
    mut parse: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(HEAD_MAX_BYTES));
    let mut line = String::new();
    for _ in 0..max_lines {
        line.clear();
        reader.read_line(&mut line).ok()?;
        if !line.ends_with('\n') {
            return None;
        }
        if let Some(answer) = parse(&line) {
            return Some(answer);
        }
    }
    None
}

/// The surface the newest recorded session of `agent_token` names, when its
/// record names one Rift knows.
fn recorded_surface(agent_token: &str, sessions: &[SessionCache]) -> Option<Surface> {
    let family = ClientFamily::from_token(agent_token)?;
    let (_, transcript) = newest_transcript(sessions, agent_token)?;
    match family {
        ClientFamily::ClaudeCode => {
            let entrypoint = first_in_head(&transcript, CLAUDE_HEAD_LINES, entrypoint_of)?;
            claude_code_surface(&entrypoint)
        }
        ClientFamily::Codex => match codex_client_at(&transcript)?.surface {
            CodexSurface::VsCode => Some(Surface::Ide(NativeIdeHost::VsCode)),
            CodexSurface::Terminal => Some(Surface::Terminal),
        },
        ClientFamily::Gemini | ClientFamily::Antigravity => None,
    }
}

/// The string `entrypoint` a Claude Code transcript record carries, if it
/// carries one.
fn entrypoint_of(line: &str) -> Option<String> {
    serde_json::from_str::<EntrypointRecord>(line)
        .ok()?
        .entrypoint
}

/// Claude Code's `entrypoint` values: the extension, or a process started
/// from a shell (the command line and the three SDKs). Any other value is no
/// answer, so presence decides rather than a guess.
fn claude_code_surface(entrypoint: &str) -> Option<Surface> {
    match entrypoint {
        "claude-vscode" => Some(Surface::Ide(NativeIdeHost::VsCode)),
        "cli" | "sdk-cli" | "sdk-ts" | "sdk-py" => Some(Surface::Terminal),
        _ => None,
    }
}

/// The one surface the presence evidence names: a single editor host's
/// extension alone, JetBrains plugins alone, or a `PATH` command with at most
/// the vendor's own install beside it. Several classes, or none, name none.
fn presence_surface(installations: &[NativeInstallation]) -> Surface {
    let has = |wanted: NativeInstallSource| {
        installations
            .iter()
            .any(|installation| installation.source == wanted)
    };
    let mut hosts: Vec<NativeIdeHost> = installations
        .iter()
        .filter_map(|installation| match installation.source {
            NativeInstallSource::IdeExtension(host) => Some(host),
            _ => None,
        })
        .collect();
    // Installations are reported host by host, so equal hosts are adjacent.
    hosts.dedup();
    let command = has(NativeInstallSource::PathCommand);
    let vendor = has(NativeInstallSource::VendorInstall);
    let jetbrains = has(NativeInstallSource::JetBrainsPlugin);
    match (command, vendor, hosts.as_slice(), jetbrains) {
        (true, _, [], false) => Surface::Terminal,
        (false, false, [host], false) => Surface::Ide(*host),
        (false, false, [], true) => Surface::JetBrains,
        _ => Surface::Unknown,
    }
}
