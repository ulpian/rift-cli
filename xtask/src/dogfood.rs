//! `cargo run -p xtask -- dogfood` — the scripted-session savings reading.
//!
//! A developer with Claude Code, Codex and Antigravity installed can play a
//! fixed set of ten short sessions through each client and read one table:
//! what the read levers were worth on that sample. Everything the harness
//! needs is in the tree — the prompts are versioned beside this module — so
//! two developers who run it play the same sessions.
//!
//! Three rules shape the design.
//!
//! 1. **The developer's own family is never touched.** Each client gets a
//!    scratch clone of this checkout, which `rift init` mints as a family of
//!    its own; the harness asserts the scratch family's stores are not the
//!    developer's before a client runs. The evaluation window on the real
//!    repository is left exactly where it was.
//! 2. **It spends real quota, so nothing runs it by accident.** It refuses to
//!    start under `CI`, says what it will spend before it spends it, and is
//!    never reachable from `cargo test`: the unit tests below cover the pure
//!    halves — the session loader, the report parsers, the table and the JSON
//!    — and drive no client.
//! 3. **A reading, not a gate.** The command exits 0 whatever the share comes
//!    out at. A non-zero exit means the harness itself failed; a client that
//!    refuses or runs out of quota is recorded as unavailable and the run
//!    carries on.
//!
//! `--compare` plays the same sessions a second time in a second checkout with
//! every read lever off, and prints the booked saving against the removal the
//! two arms measure; [`crate::dogfood_compare`] holds that half. Without it
//! the run is what it was, byte for byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dogfood_compare::{compare_json, render_compare_table, Arm, CompareRun, ARMS};

/// The three clients this harness can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Client {
    /// Claude Code (`claude`).
    ClaudeCode,
    /// Codex (`codex`).
    Codex,
    /// Antigravity (`agy`).
    Antigravity,
}

/// Every client, in the order the table prints them.
pub const ALL_CLIENTS: [Client; 3] = [Client::ClaudeCode, Client::Codex, Client::Antigravity];

impl Client {
    /// The name on the command line, and the name `rift enable --agent` wants.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }

    /// The executable this client is driven through.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
        }
    }

    /// Parse the `--client` value: one slug, or `all`.
    ///
    /// # Errors
    /// A value that is neither `all` nor one of the three slugs.
    pub fn parse(value: &str) -> Result<Vec<Self>, String> {
        if value == "all" {
            return Ok(ALL_CLIENTS.to_vec());
        }
        ALL_CLIENTS
            .iter()
            .find(|c| c.slug() == value)
            .map(|c| vec![*c])
            .ok_or_else(|| {
                format!("dogfood: unknown client `{value}` — one of claude-code, codex, antigravity, all")
            })
    }

    /// The argument vector that plays one session headlessly.
    ///
    /// The flags are the least-permissive set that still lets the client read,
    /// edit, glob and grep in the scratch checkout, and each one asks for the
    /// client's own structured event stream so the log can be parsed
    /// afterwards. `repo` is the scratch checkout, absolute.
    #[must_use]
    pub fn argv(self, prompt: &str, repo: &Path) -> Vec<String> {
        let repo = repo.display().to_string();
        match self {
            // `--setting-sources project,local` confines settings to the
            // scratch checkout — `rift enable` writes the wiring into
            // `.claude/settings.local.json`, and the developer's own user
            // settings stay out of the run.
            Self::ClaudeCode => vec![
                "-p".to_owned(),
                prompt.to_owned(),
                "--setting-sources".to_owned(),
                "project,local".to_owned(),
                "--allowedTools".to_owned(),
                "Read".to_owned(),
                "Edit".to_owned(),
                "Glob".to_owned(),
                "Grep".to_owned(),
                "--permission-mode".to_owned(),
                "acceptEdits".to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--verbose".to_owned(),
            ],
            // `--dangerously-bypass-hook-trust` runs the checkout's hook file
            // for this invocation and persists nothing; the approvals-and-
            // sandbox bypass is deliberately NOT used, because that one writes
            // a trust row into the developer's global client configuration.
            Self::Codex => vec![
                "exec".to_owned(),
                "--skip-git-repo-check".to_owned(),
                "--dangerously-bypass-hook-trust".to_owned(),
                "--sandbox".to_owned(),
                "workspace-write".to_owned(),
                "--json".to_owned(),
                "--color".to_owned(),
                "never".to_owned(),
                prompt.to_owned(),
            ],
            // `--new-project` is required rather than tidy: without it this
            // client continues the previous conversation and the sessions stop
            // being independent.
            Self::Antigravity => vec![
                "-p".to_owned(),
                prompt.to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--dangerously-skip-permissions".to_owned(),
                "--add-dir".to_owned(),
                repo,
                "--new-project".to_owned(),
            ],
        }
    }
}

// --- the scripted sessions ---------------------------------------------------

/// The tracked directory the session prompts live in, relative to the root.
pub const SESSIONS_DIR: &str = "xtask/dogfood/sessions";

/// The first line of a session file: its one-line statement of intent.
const INTENT_OPEN: &str = "<!-- intent:";
/// How that line closes.
const INTENT_CLOSE: &str = "-->";

/// One scripted session: a file name, an intent, and the prompt itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The two-digit ordinal from the file name.
    pub number: u32,
    /// The file's stem, which is also the log file's name.
    pub name: String,
    /// The one-line header saying what this session is shaped like.
    pub intent: String,
    /// The prompt handed to the client, verbatim.
    pub prompt: String,
}

/// Parse one session file.
///
/// # Errors
/// A file name that is not `NN-name.md`, a missing intent header, or an empty
/// prompt body — each of which would otherwise play as a session that measures
/// nothing.
pub fn parse_session(file_name: &str, text: &str) -> Result<Session, String> {
    let stem = file_name
        .strip_suffix(".md")
        .ok_or_else(|| format!("dogfood: {file_name}: a session file must end in .md"))?;
    let (digits, rest) = stem.split_at(stem.len().min(2));
    let number: u32 = digits
        .parse()
        .map_err(|_| format!("dogfood: {file_name}: must start with a two-digit ordinal"))?;
    if !rest.starts_with('-') || rest.len() < 2 {
        return Err(format!(
            "dogfood: {file_name}: the ordinal must be followed by `-` and a name"
        ));
    }
    let mut lines = text.lines();
    let header = lines
        .next()
        .unwrap_or_default()
        .trim()
        .strip_prefix(INTENT_OPEN)
        .and_then(|r| r.strip_suffix(INTENT_CLOSE))
        .map(str::trim)
        .ok_or_else(|| {
            format!("dogfood: {file_name}: the first line must be `{INTENT_OPEN} … {INTENT_CLOSE}`")
        })?;
    if header.is_empty() {
        return Err(format!("dogfood: {file_name}: the intent header is empty"));
    }
    let prompt = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if prompt.is_empty() {
        return Err(format!("dogfood: {file_name}: the prompt body is empty"));
    }
    Ok(Session {
        number,
        name: stem.to_owned(),
        intent: header.to_owned(),
        prompt,
    })
}

/// Load every session in `dir`, ordered by file name.
///
/// # Errors
/// An unreadable directory, an unparsable file, or a duplicate ordinal.
pub fn load_sessions(dir: &Path) -> Result<Vec<Session>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("dogfood: read {}: {e}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    let mut out = Vec::with_capacity(files.len());
    let mut seen: BTreeMap<u32, String> = BTreeMap::new();
    for path in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("dogfood: read {}: {e}", path.display()))?;
        let session = parse_session(&name, &text)?;
        if let Some(first) = seen.insert(session.number, name.clone()) {
            return Err(format!(
                "dogfood: {name}: ordinal {} is already used by {first}",
                session.number
            ));
        }
        out.push(session);
    }
    if out.is_empty() {
        return Err(format!(
            "dogfood: no session files in {} — the harness would measure nothing",
            dir.display()
        ));
    }
    Ok(out)
}

// --- reading what Rift recorded ----------------------------------------------

/// The figures one `rift savings --json` call reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavingsSample {
    /// `avoided ÷ (avoided + read)`, or `None` when neither side has a token.
    pub share_pct: Option<f64>,
    /// Σ avoided over the audit relation.
    pub tokens_avoided: u64,
    /// The prevented part of `tokens_avoided`, when the reporting surface
    /// carries it — never a second figure beside it, so nothing adds the two.
    /// `None` means the surface did not report it, which prints as `-` rather
    /// than as a zero the harness cannot defend.
    pub tokens_prevented: Option<u64>,
    /// Σ returned over the same relation.
    pub tokens_returned: u64,
    /// Every read token the capture projection holds for this family.
    pub read_tokens: u64,
    /// `avoided − returned`, signed.
    pub honesty: i64,
    /// The headline's `n`.
    pub sessions_with_reads: u64,
    /// The window the figures cover, as the command labelled it.
    pub window: String,
    /// The capture projection the read side came from.
    pub projection_db: String,
    /// The recall projection the avoided side came from.
    pub recall_db: String,
    /// Why the figures are absent, when they are.
    pub note: Option<String>,
}

/// Read a `rift savings --json` document.
///
/// # Errors
/// Output that is not JSON, or JSON without the `overall` block — either of
/// which means the harness is reading something other than the command it
/// thinks it ran, and a silently-zero table is worse than a refusal.
pub fn parse_savings(json: &str) -> Result<SavingsSample, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("dogfood: savings is not JSON: {e}"))?;
    let overall = v
        .get("overall")
        .ok_or_else(|| "dogfood: savings JSON has no `overall` block".to_owned())?;
    let u = |k: &str| {
        overall
            .get(k)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Ok(SavingsSample {
        share_pct: overall.get("share_pct").and_then(serde_json::Value::as_f64),
        tokens_avoided: u("tokens_avoided"),
        // Absent and zero are different answers: a surface that does not
        // report the prevented part must not be read as one that reports none.
        tokens_prevented: overall
            .get("tokens_prevented")
            .and_then(serde_json::Value::as_u64),
        tokens_returned: u("tokens_returned"),
        read_tokens: u("read_tokens"),
        honesty: overall
            .get("honesty")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        sessions_with_reads: u("sessions_with_reads"),
        window: v
            .pointer("/window/boundary_source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(unlabelled)")
            .to_owned(),
        projection_db: text_at(&v, "projection_db"),
        recall_db: text_at(&v, "recall_db"),
        note: v
            .get("note")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// One top-level string field, or the empty string.
fn text_at(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What one session moved a family's figures by: the reading after it, minus
/// the reading before it.
///
/// Every axis is signed. The figures are cumulative counters over an
/// append-only relation, so a session normally moves each of them up — but a
/// superseding correction can move one down, and a delta that clamped at zero
/// would hide exactly the case a calibration is run to find.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SavingsDeltas {
    /// Read tokens booked across the session.
    pub read_tokens: i64,
    /// Avoided tokens booked across the session.
    pub tokens_avoided: i64,
    /// The prevented part of those, when both readings carried it.
    pub tokens_prevented: Option<i64>,
    /// Returned tokens booked across the session.
    pub tokens_returned: i64,
}

impl SavingsDeltas {
    /// `after − before`, on every axis.
    ///
    /// The prevented axis is `None` unless **both** readings carried the
    /// figure: a delta against an absent side would be the surface's silence
    /// read as a number.
    #[must_use]
    pub fn between(before: &SavingsSample, after: &SavingsSample) -> Self {
        let d = |after: u64, before: u64| {
            i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(i64::MAX)
        };
        Self {
            read_tokens: d(after.read_tokens, before.read_tokens),
            tokens_avoided: d(after.tokens_avoided, before.tokens_avoided),
            tokens_prevented: after
                .tokens_prevented
                .zip(before.tokens_prevented)
                .map(|(a, b)| d(a, b)),
            tokens_returned: d(after.tokens_returned, before.tokens_returned),
        }
    }
}

/// What a decision was about, as the table groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DecisionKind {
    /// A read the gate answered from what the window already holds.
    Intercept,
    /// A document read pointed at the Markdown beside it.
    Document,
    /// A content search bounded or answered.
    Search,
    /// Everything else the audit relation records.
    Other,
}

impl DecisionKind {
    /// The column label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Intercept => "intercept",
            Self::Document => "document",
            Self::Search => "search",
            Self::Other => "other",
        }
    }
}

/// Which column a `memd.*` verb belongs in.
///
/// Grouped by the lever that produced the row rather than by the verb's exact
/// spelling, so a new verb inside an existing lever lands in the right column
/// without a table edit; anything genuinely new shows up under `other` rather
/// than being folded into a lever it does not belong to.
#[must_use]
pub fn classify_verb(verb: &str) -> DecisionKind {
    let bare = verb.strip_prefix("memd.").unwrap_or(verb);
    if bare.starts_with("read_")
        || bare.starts_with("recovery_")
        || bare.starts_with("manifest_")
        || bare.starts_with("summary_")
        || bare == "known_state"
    {
        DecisionKind::Intercept
    } else if bare.starts_with("doc_") {
        DecisionKind::Document
    } else if bare.starts_with("search") || bare.starts_with("grep_") {
        DecisionKind::Search
    } else {
        DecisionKind::Other
    }
}

/// One row of the audit relation's `GROUP BY verb`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeverKind {
    /// The verb the rows carry.
    pub verb: String,
    /// How many rows there are.
    pub rows: u64,
    /// What those decisions cost.
    pub tokens_returned: u64,
    /// What those decisions avoided.
    pub tokens_avoided: u64,
}

/// Read the `levers.kinds` block of a `rift memory reread-report --json`.
///
/// # Errors
/// Output that is not JSON. A report with no `levers.kinds` array is not an
/// error — a family that has decided nothing has no kinds — and comes back
/// empty.
pub fn parse_kinds(json: &str) -> Result<Vec<LeverKind>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("dogfood: the reread report is not JSON: {e}"))?;
    let Some(rows) = v
        .pointer("/levers/kinds")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(LeverKind {
                verb: r.get("verb")?.as_str()?.to_owned(),
                rows: r
                    .get("rows")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                tokens_returned: r
                    .get("tokens_returned")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                tokens_avoided: r
                    .get("tokens_avoided")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect())
}

/// Rows per decision kind, in the table's column order.
#[must_use]
pub fn tally_kinds(kinds: &[LeverKind]) -> BTreeMap<DecisionKind, u64> {
    let mut out = BTreeMap::new();
    for k in kinds {
        *out.entry(classify_verb(&k.verb)).or_insert(0) += k.rows;
    }
    out
}

// --- what the client's own log exposes ---------------------------------------

/// The marker every line Rift writes into a model's context opens with. It is
/// the one token the three clients pass through unchanged, so it is what a log
/// is searched for.
const RIFT_MARKER: &str = "[rift]";

/// The tool-call fields a client's event stream names its target in.
const TARGET_KEYS: [&str; 4] = ["file_path", "AbsolutePath", "command", "pattern"];

/// Denials seen in one client's log, and how often the model asked again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetryCounts {
    /// Lines carrying a Rift decision the model could read.
    pub denies: u64,
    /// Of those, the ones whose target the model named again afterwards.
    pub retries: u64,
    /// Whether the log named tool targets at all. A client whose headless
    /// output does not expose them reports `n/a` rather than a zero it cannot
    /// defend.
    pub observed: bool,
}

/// Count retry-after-deny over one client's structured log.
///
/// The heuristic, stated so a reader can disagree with it: a log line carrying
/// the marker is a decision the model was shown, and every tool target named
/// on that line is a target that was declined; the next line that names the
/// same target without the marker is the model asking again. It is a lower
/// bound — a client that never names its targets reports nothing rather than
/// zero — and it is the same shape the archived spikes counted by hand.
#[must_use]
pub fn retries_after_deny(log: &str) -> RetryCounts {
    let mut out = RetryCounts::default();
    let mut pending: Vec<String> = Vec::new();
    for line in log.lines() {
        let targets = targets_on(line);
        if !targets.is_empty() {
            out.observed = true;
        }
        if line.contains(RIFT_MARKER) {
            for t in targets {
                if !pending.contains(&t) {
                    pending.push(t);
                    out.denies += 1;
                }
            }
            continue;
        }
        for t in targets {
            if let Some(at) = pending.iter().position(|p| *p == t) {
                pending.remove(at);
                out.retries += 1;
            }
        }
    }
    out
}

/// Every tool target named on one line of a client's event stream.
///
/// A scan over the raw text rather than a parse, because the three clients
/// nest these fields differently and at different depths, and the question
/// here is only "which file or command does this line name".
fn targets_on(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in TARGET_KEYS {
        let needle = format!("\"{key}\":");
        let mut from = 0usize;
        while let Some(at) = line.get(from..).and_then(|s| s.find(&needle)) {
            let after = from + at + needle.len();
            from = after;
            let Some(rest) = line.get(after..) else { break };
            let rest = rest.trim_start();
            let Some(body) = rest.strip_prefix('"') else {
                continue;
            };
            let mut value = String::new();
            let mut escaped = false;
            for ch in body.chars() {
                if escaped {
                    value.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                } else {
                    value.push(ch);
                }
            }
            if !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

// --- the run itself -----------------------------------------------------------

/// How one scripted session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The client ran the session to completion.
    Completed,
    /// The client refused, was missing, or ran out of quota.
    Unavailable,
    /// The time box fired first.
    TimedOut,
    /// The client exited non-zero for some other reason.
    Failed,
}

impl Status {
    /// The word the table and the JSON both use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Unavailable => "UNAVAILABLE",
            Self::TimedOut => "TIMED OUT",
            Self::Failed => "FAILED",
        }
    }
}

/// One played session and what Rift recorded for it.
#[derive(Clone, Debug)]
pub struct SessionOutcome {
    /// The session's ordinal.
    pub number: u32,
    /// The session's name.
    pub name: String,
    /// How it ended.
    pub status: Status,
    /// The client's exit code, when it produced one.
    pub exit_code: Option<i32>,
    /// Wall-clock seconds.
    pub seconds: u64,
    /// `rift savings --json` for the scratch family, after this session.
    pub savings: Option<SavingsSample>,
    /// What this session alone moved that family's figures by.
    pub deltas: SavingsDeltas,
    /// What the client's own event stream said the session cost it, where the
    /// stream carries the figure at all.
    pub client_reported: Option<u64>,
    /// Lines in the checkout's read ledgers after this session.
    pub ledger_lines: u64,
    /// What the client's own log exposed.
    pub retries: RetryCounts,
    /// The first line of whatever went wrong.
    pub error: Option<String>,
}

/// Everything one client's ten sessions produced.
#[derive(Clone, Debug)]
pub struct ClientRun {
    /// The client.
    pub client: Client,
    /// The scratch checkout it played in.
    pub scratch: PathBuf,
    /// The scratch family's recall projection — printed so a reader can see it
    /// is not the developer's.
    pub recall_db: String,
    /// The scratch family's capture projection.
    pub projection_db: String,
    /// One entry per scripted session.
    pub sessions: Vec<SessionOutcome>,
    /// The savings figure after the last session.
    pub final_savings: Option<SavingsSample>,
    /// The audit relation's rows by verb, at the end.
    pub kinds: Vec<LeverKind>,
}

impl ClientRun {
    /// Sessions the client actually completed.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status == Status::Completed)
            .count()
    }

    /// Retry counts summed over the sessions whose logs exposed them.
    #[must_use]
    pub fn retries(&self) -> RetryCounts {
        let mut out = RetryCounts::default();
        for s in &self.sessions {
            if s.retries.observed {
                out.observed = true;
                out.denies += s.retries.denies;
                out.retries += s.retries.retries;
            }
        }
        out
    }
}

/// A share as the surfaces spell it, or the word for a question with no answer.
#[must_use]
pub fn fmt_share(share: Option<f64>) -> String {
    share.map_or_else(|| "n/a".to_owned(), |s| format!("{s:.2}%"))
}

/// Left-pad to `width` columns.
fn pad(label: &str, width: usize) -> String {
    let mut out = label.to_owned();
    for _ in label.chars().count()..width {
        out.push(' ');
    }
    out
}

/// The one table the command prints: a row per client, then the overall row.
#[must_use]
pub fn render_table(runs: &[ClientRun]) -> String {
    let mut out = String::new();
    out.push_str(
        "dogfood — the read levers over a fixed scripted sample, through the real clients\n",
    );
    out.push_str(&format!(
        "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
        pad("client", 14),
        "done",
        "n",
        "read",
        "avoided",
        "returned",
        "honesty",
        "share"
    ));
    let (mut avoided, mut returned, mut read, mut n, mut done, mut planned) =
        (0u64, 0u64, 0u64, 0u64, 0usize, 0usize);
    for r in runs {
        let s = r.final_savings.clone().unwrap_or_default();
        avoided = avoided.saturating_add(s.tokens_avoided);
        returned = returned.saturating_add(s.tokens_returned);
        read = read.saturating_add(s.read_tokens);
        n = n.saturating_add(s.sessions_with_reads);
        done += r.completed();
        planned += r.sessions.len();
        out.push_str(&format!(
            "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
            pad(r.client.slug(), 14),
            format!("{}/{}", r.completed(), r.sessions.len()),
            s.sessions_with_reads,
            s.read_tokens,
            s.tokens_avoided,
            s.tokens_returned,
            s.honesty,
            fmt_share(s.share_pct)
        ));
    }
    out.push_str(&format!(
        "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
        pad("overall", 14),
        format!("{done}/{planned}"),
        n,
        read,
        avoided,
        returned,
        i64::try_from(avoided).unwrap_or(i64::MAX) - i64::try_from(returned).unwrap_or(i64::MAX),
        fmt_share(share_of(avoided, read))
    ));
    out.push_str("\ndecisions by kind (rows in the audit relation), and retry-after-deny\n");
    out.push_str(&format!(
        "  {}{:>12}{:>12}{:>10}{:>10}{:>18}\n",
        pad("client", 14),
        "intercept",
        "document",
        "search",
        "other",
        "retry/deny"
    ));
    for r in runs {
        let t = tally_kinds(&r.kinds);
        let get = |k: DecisionKind| t.get(&k).copied().unwrap_or(0);
        let retries = r.retries();
        let retry = if retries.observed {
            format!("{}/{}", retries.retries, retries.denies)
        } else {
            "n/a".to_owned()
        };
        out.push_str(&format!(
            "  {}{:>12}{:>12}{:>10}{:>10}{:>18}\n",
            pad(r.client.slug(), 14),
            get(DecisionKind::Intercept),
            get(DecisionKind::Document),
            get(DecisionKind::Search),
            get(DecisionKind::Other),
            retry
        ));
    }
    out.push_str("\nsessions that did not complete\n");
    let mut any = false;
    for r in runs {
        for s in &r.sessions {
            if s.status == Status::Completed {
                continue;
            }
            any = true;
            out.push_str(&format!(
                "  {} {} — {}: {}\n",
                pad(r.client.slug(), 14),
                s.name,
                s.status.label(),
                s.error.clone().unwrap_or_else(|| "no detail".to_owned())
            ));
        }
    }
    if !any {
        out.push_str("  (none)\n");
    }
    out
}

/// `a ÷ (a + b)` as a percentage, `None` when both sides are empty.
#[must_use]
pub fn share_of(avoided: u64, read: u64) -> Option<f64> {
    let total = avoided.saturating_add(read);
    if total == 0 {
        None
    } else {
        Some(avoided as f64 * 100.0 / total as f64)
    }
}

/// The same facts as the table, as one JSON document.
#[must_use]
pub fn report_json(runs: &[ClientRun], stamp: &str) -> serde_json::Value {
    let clients: Vec<serde_json::Value> = runs
        .iter()
        .map(|r| {
            let s = r.final_savings.clone().unwrap_or_default();
            let t = tally_kinds(&r.kinds);
            let retries = r.retries();
            serde_json::json!({
                "client": r.client.slug(),
                "scratch": r.scratch.display().to_string(),
                "recall_db": r.recall_db,
                "projection_db": r.projection_db,
                "sessions_played": r.sessions.len(),
                "sessions_completed": r.completed(),
                "savings": {
                    "share_pct": s.share_pct,
                    "tokens_avoided": s.tokens_avoided,
                    "tokens_returned": s.tokens_returned,
                    "read_tokens": s.read_tokens,
                    "honesty": s.honesty,
                    "sessions_with_reads": s.sessions_with_reads,
                    "window": s.window,
                    "note": s.note,
                },
                "decisions_by_kind": {
                    "intercept": t.get(&DecisionKind::Intercept).copied().unwrap_or(0),
                    "document": t.get(&DecisionKind::Document).copied().unwrap_or(0),
                    "search": t.get(&DecisionKind::Search).copied().unwrap_or(0),
                    "other": t.get(&DecisionKind::Other).copied().unwrap_or(0),
                },
                "verbs": r.kinds.iter().map(|k| serde_json::json!({
                    "verb": k.verb, "rows": k.rows,
                    "tokens_avoided": k.tokens_avoided, "tokens_returned": k.tokens_returned,
                })).collect::<Vec<_>>(),
                "retry_after_deny": if retries.observed {
                    serde_json::json!({ "denies": retries.denies, "retries": retries.retries })
                } else {
                    serde_json::json!({ "observed": false })
                },
                "sessions": r.sessions.iter().map(|s| serde_json::json!({
                    "number": s.number,
                    "name": s.name,
                    "status": s.status.label(),
                    "exit_code": s.exit_code,
                    "seconds": s.seconds,
                    "ledger_lines": s.ledger_lines,
                    "error": s.error,
                    "savings": s.savings.as_ref().map(|v| serde_json::json!({
                        "share_pct": v.share_pct,
                        "tokens_avoided": v.tokens_avoided,
                        "tokens_returned": v.tokens_returned,
                        "read_tokens": v.read_tokens,
                        "honesty": v.honesty,
                        "sessions_with_reads": v.sessions_with_reads,
                    })),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let avoided: u64 = runs
        .iter()
        .map(|r| r.final_savings.clone().unwrap_or_default().tokens_avoided)
        .sum();
    let read: u64 = runs
        .iter()
        .map(|r| r.final_savings.clone().unwrap_or_default().read_tokens)
        .sum();
    serde_json::json!({
        "kind": "dogfood-reading",
        "definition": "avoided / (avoided + read) over a fixed scripted sample, per client",
        "caveat": "a reading over ten scripted sessions through real clients — \
                   non-deterministic, not a gate, and not the class-sized fixture",
        "stamp": stamp,
        "overall_share_pct": share_of(avoided, read),
        "clients": clients,
    })
}

/// `YYYYMMDD-HHMMSS`, UTC, from seconds since the epoch.
///
/// Spelled out rather than pulled from a date crate: this crate carries two
/// dependencies and a run stamp is not worth a third.
#[must_use]
pub fn utc_stamp(epoch_secs: u64) -> String {
    let days = epoch_secs / 86_400;
    let secs = epoch_secs % 86_400;
    // Days from 1970-01-01 to a civil date, by the shifted-era algorithm: the
    // year starts in March so a leap day is always last.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

// --- the command line ---------------------------------------------------------

/// How many minutes one scripted session may run before the box fires.
const SESSION_MINUTES: u64 = 10;

/// What one invocation was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The clients to play, in table order.
    pub clients: Vec<Client>,
    /// How many of the scripted sessions to play, from the first.
    pub sessions: usize,
    /// Where the scratch checkouts and logs go. `None` means the default
    /// under the home directory, stamped with the run's own UTC instant.
    pub out: Option<PathBuf>,
    /// Keep the scratch checkouts (and their registry rows) afterwards.
    pub keep: bool,
    /// Play every session twice — once with every read lever off, once as
    /// they ship — and print the calibration between the two arms. It doubles
    /// what the run spends.
    pub compare: bool,
}

/// Parse the argv after the `dogfood` token.
///
/// # Errors
/// A missing or unknown `--client`, a flag without its value, or an
/// unrecognised flag.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    const USAGE: &str = "usage: xtask dogfood --client <claude-code|codex|antigravity|all> \
                         [--sessions N] [--out DIR] [--keep] [--compare]";
    let mut clients = None;
    let mut sessions = 10usize;
    let mut out = None;
    let mut keep = false;
    let mut compare = false;
    let mut at = 0usize;
    while let Some(arg) = args.get(at) {
        let value = |name: &str| -> Result<String, String> {
            args.get(at + 1)
                .cloned()
                .ok_or_else(|| format!("dogfood: {name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--client" => {
                clients = Some(Client::parse(&value("--client")?)?);
                at += 2;
            }
            "--sessions" => {
                sessions = value("--sessions")?
                    .parse()
                    .map_err(|_| format!("dogfood: --sessions wants a number\n{USAGE}"))?;
                at += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(value("--out")?));
                at += 2;
            }
            "--keep" => {
                keep = true;
                at += 1;
            }
            "--compare" => {
                compare = true;
                at += 1;
            }
            other => return Err(format!("dogfood: unknown argument `{other}`\n{USAGE}")),
        }
    }
    let clients = clients.ok_or_else(|| format!("dogfood: --client is required\n{USAGE}"))?;
    if sessions == 0 {
        return Err(format!("dogfood: --sessions must be at least 1\n{USAGE}"));
    }
    Ok(Options {
        clients,
        sessions,
        out,
        keep,
        compare,
    })
}

/// Whether a client's own output says it could not run at all.
///
/// Quota and authentication are the two refusals worth telling apart from a
/// bad session: they mean the reading is short by that many sessions, not that
/// the sample said something. The phrases are the ones the three clients
/// actually print.
#[must_use]
pub fn unavailability(log: &str) -> Option<String> {
    // Deliberately NOT here: "command not found". A client whose executable is
    // missing fails to launch and is reported through that path; the phrase in
    // a log is the MODEL's own shell command failing, which is an ordinary
    // thing to happen inside a session and cost a whole run's Codex leg the
    // first time this ran.
    const PHRASES: [&str; 7] = [
        "quota reached",
        "quota exceeded",
        "insufficient_quota",
        "usage limit",
        "rate limit",
        "Please run /login",
        "not authenticated",
    ];
    let lower = log.to_lowercase();
    PHRASES
        .iter()
        .find(|p| lower.contains(&p.to_lowercase()))
        .map(|p| {
            let at = lower.find(&p.to_lowercase()).unwrap_or(0);
            let line = log
                .get(at.saturating_sub(120)..(at + 200).min(log.len()))
                .unwrap_or(p)
                .lines()
                .find(|l| l.to_lowercase().contains(&p.to_lowercase()))
                .unwrap_or(p);
            line.trim().chars().take(200).collect::<String>()
        })
}

// --- driving the machine ------------------------------------------------------

/// Run a program to completion with `PATH` set, capturing both streams.
fn capture_with_path(
    program: &Path,
    args: &[String],
    cwd: &Path,
    path: &str,
) -> Result<(Option<i32>, String, String), String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .output()
        .map_err(|e| format!("dogfood: run {}: {e}", program.display()))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Run a program to completion, capturing both streams.
fn capture(
    program: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<(Option<i32>, String, String), String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("dogfood: run {}: {e}", program.display()))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Run a program and refuse if it did not exit 0.
fn must(program: &Path, args: &[String], cwd: &Path) -> Result<String, String> {
    let (code, stdout, stderr) = capture(program, args, cwd)?;
    if code == Some(0) {
        return Ok(stdout);
    }
    Err(format!(
        "dogfood: {} {} exited {:?}\n{stderr}",
        program.display(),
        args.join(" "),
        code
    ))
}

/// Where a client's executable is, preferring a version manager's copy when
/// the machine has two — the archived measurements found a second, much older
/// build first on `PATH` with no hook surface at all.
fn resolve_program(name: &str) -> Result<PathBuf, String> {
    let out = Command::new("/usr/bin/which")
        .arg("-a")
        .arg(name)
        .output()
        .map_err(|e| format!("dogfood: which {name}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut paths: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    paths.dedup();
    let preferred = paths
        .iter()
        .find(|p| p.contains("/.nvm/"))
        .or_else(|| paths.first());
    preferred
        .map(|p| PathBuf::from(*p))
        .ok_or_else(|| format!("dogfood: `{name}` is not on PATH"))
}

/// The developer's own release binary, built if it is not there yet. It is
/// the whole of what a dogfood run needs: document conversion is the same
/// binary re-spawned, not a sibling to build beside it.
fn release_binary(root: &Path) -> Result<PathBuf, String> {
    let rift = root.join("target/release/rift");
    if !rift.exists() {
        println!("dogfood: building target/release/rift …");
        must(
            Path::new("cargo"),
            &[
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "rift".to_owned(),
            ],
            root,
        )?;
    }
    if !rift.exists() {
        return Err("dogfood: the release binary is still missing after a build".to_owned());
    }
    Ok(rift)
}

/// Lines across every read ledger in a checkout.
#[must_use]
pub fn ledger_lines(scratch: &Path) -> u64 {
    let dir = scratch.join(".rift").join("state").join("reads");
    let mut total = 0u64;
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|x| x == "jsonl") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                total = total.saturating_add(text.lines().count() as u64);
            }
        }
    }
    total
}

/// Play one session, time-boxed. This machine has no `timeout` binary, so the
/// box is `perl`'s alarm, which is what the archived measurements used.
fn play(
    client: Client,
    program: &Path,
    rift: &Path,
    session: &Session,
    scratch: &Path,
    log_dir: &Path,
) -> SessionOutcome {
    let mut argv = vec![
        "-e".to_owned(),
        "alarm shift; exec @ARGV".to_owned(),
        (SESSION_MINUTES * 60).to_string(),
        program.display().to_string(),
    ];
    argv.extend(client.argv(&session.prompt, scratch));
    let started = std::time::Instant::now();
    // The frames Rift injects name `rift` as a command to run. On a developer's
    // machine the binary is on PATH; here it is a build artifact, and a session
    // that cannot run the verb it was pointed at measures a broken install
    // rather than a lever. So the release binary's own directory leads PATH for
    // the client and for anything the model runs inside it.
    let path = rift.parent().map_or_else(
        || std::env::var("PATH").unwrap_or_default(),
        |dir| {
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            )
        },
    );
    let result = capture_with_path(Path::new("/usr/bin/perl"), &argv, scratch, &path);
    let seconds = started.elapsed().as_secs();
    let mut outcome = SessionOutcome {
        number: session.number,
        name: session.name.clone(),
        status: Status::Failed,
        exit_code: None,
        seconds,
        savings: None,
        deltas: SavingsDeltas::default(),
        client_reported: None,
        ledger_lines: 0,
        retries: RetryCounts::default(),
        error: None,
    };
    let (code, stdout, stderr) = match result {
        Ok(v) => v,
        Err(e) => {
            outcome.status = Status::Unavailable;
            outcome.error = Some(e);
            return outcome;
        }
    };
    let stem = &session.name;
    let _ = std::fs::write(log_dir.join(format!("{stem}.stream.jsonl")), &stdout);
    let _ = std::fs::write(log_dir.join(format!("{stem}.err.txt")), &stderr);
    let both = format!("{stdout}\n{stderr}");
    outcome.exit_code = code;
    outcome.retries = retries_after_deny(&both);
    outcome.client_reported = crate::dogfood_compare::client_reported_tokens(client, &stdout);
    outcome.status = match code {
        Some(0) => Status::Completed,
        // A time box fires as a signal, so the child has no code at all.
        None => Status::TimedOut,
        Some(_) => Status::Failed,
    };
    if let Some(reason) = unavailability(&both) {
        outcome.status = Status::Unavailable;
        outcome.error = Some(reason);
    } else if outcome.status != Status::Completed {
        outcome.error = Some(
            stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no stderr")
                .chars()
                .take(200)
                .collect::<String>(),
        );
    }
    outcome
}

/// The two documents the scripted sessions read, copied (never moved, never
/// edited) out of the converter's own corpus.
const DOCUMENTS: [(&str, &str); 2] = [
    (
        "crates/rift-recall-doc/fixtures/corpus/spec.docx",
        "docs/reference/spec.docx",
    ),
    (
        "crates/rift-recall-doc/fixtures/corpus/text-layer.pdf",
        "docs/reference/overview.pdf",
    ),
];

/// Refuse a scratch family that is already being measured.
///
/// The developer's own family is the first thing in `seen`, because a reading
/// that wrote into somebody's real evaluation window would be worth nothing.
/// Every family this run has already minted follows it: with two arms the
/// whole question is whether the levers moved anything, and two arms sharing
/// one ledger would answer it by construction.
///
/// # Errors
/// A family whose capture or recall projection is one of `seen`.
pub fn refuse_shared_family(
    theirs: &SavingsSample,
    seen: &[(String, String)],
) -> Result<(), String> {
    for (recall_db, projection_db) in seen {
        if theirs.recall_db == *recall_db || theirs.projection_db == *projection_db {
            return Err(format!(
                "dogfood: the scratch checkout resolved to a family that is already \
                 being measured ({}) — refusing to measure into it",
                theirs.recall_db
            ));
        }
    }
    Ok(())
}

/// The two readings one preparation takes, and which question each answers.
///
/// They are two readings rather than one because the preparation does work of
/// its own between them: the shared-family refusal has to happen before
/// anything is enabled, while the priming of the documents — four `doc read`
/// calls — books rows of its own, and a session measured against the earlier
/// reading would carry that priming as if the client had done it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    /// The reading taken right after `init`, before `enable`.
    minted: SavingsSample,
    /// The reading taken at the end of the preparation, after the priming.
    primed: SavingsSample,
}

impl Prepared {
    /// Both readings, in the order the preparation took them.
    #[must_use]
    pub const fn new(minted: SavingsSample, primed: SavingsSample) -> Self {
        Self { minted, primed }
    }

    /// The reading the shared-family refusal is made against.
    ///
    /// It is the first one, taken while the checkout is still untouched: a
    /// clone that resolved to a family already being measured must be refused
    /// before the preparation writes a single row into it.
    #[must_use]
    pub const fn minted(&self) -> &SavingsSample {
        &self.minted
    }

    /// The reading a session's delta is measured against.
    ///
    /// It is the last one, so the first session's figures are the first
    /// session's own and not the preparation's priming as well.
    #[must_use]
    pub const fn baseline(&self) -> &SavingsSample {
        &self.primed
    }
}

/// Prepare one client's scratch checkout: clone, documents, `init`, `enable`,
/// the release binary, and the assertion that this is not the developer's
/// family.
fn prepare(
    root: &Path,
    rift: &Path,
    client: Client,
    scratch: &Path,
    seen: &[(String, String)],
    arm: Option<Arm>,
) -> Result<Prepared, String> {
    let git = Path::new("/usr/bin/git");
    must(
        git,
        &[
            "clone".to_owned(),
            "--shared".to_owned(),
            "--quiet".to_owned(),
            root.display().to_string(),
            scratch.display().to_string(),
        ],
        root,
    )?;
    std::fs::create_dir_all(scratch.join("docs/reference"))
        .map_err(|e| format!("dogfood: docs/reference: {e}"))?;
    for (from, to) in DOCUMENTS {
        std::fs::copy(root.join(from), scratch.join(to))
            .map_err(|e| format!("dogfood: copy {from}: {e}"))?;
    }
    must(
        rift,
        &[
            "init".to_owned(),
            "--skip-splash".to_owned(),
            "--quiet".to_owned(),
        ],
        scratch,
    )?;
    // The family the scratch checkout minted must be its own — not the
    // developer's, and not the other arm's.
    let theirs = parse_savings(&must(
        rift,
        &["savings".to_owned(), "--json".to_owned()],
        scratch,
    )?)?;
    refuse_shared_family(&theirs, seen)?;
    must(
        rift,
        &[
            "enable".to_owned(),
            "--agent".to_owned(),
            client.slug().to_owned(),
        ],
        scratch,
    )?;
    must(
        rift,
        &[
            "hooks".to_owned(),
            "install".to_owned(),
            "--repoint".to_owned(),
            "--binary".to_owned(),
            rift.display().to_string(),
        ],
        scratch,
    )?;
    // A document is converted in the background the first time it is read, and
    // that first read is past the gate's deadline: it journals the conversion
    // and runs untouched. So the priming here is the whole sequence a
    // developer who opened both files once would have driven — ask, drain the
    // journal, ask again — because a client that meets an unconverted document
    // measures a race rather than a lever.
    let read_both = || {
        for (_, to) in DOCUMENTS {
            let _ = capture(
                rift,
                &["doc".to_owned(), "read".to_owned(), to.to_owned()],
                scratch,
            );
        }
    };
    read_both();
    let _ = capture(
        rift,
        &[
            "hooks".to_owned(),
            "worker".to_owned(),
            "--git-dir".to_owned(),
            scratch.join(".git").display().to_string(),
        ],
        scratch,
    );
    read_both();
    let sidecars = scratch.join(".rift").join("state").join("doc");
    let converted = std::fs::read_dir(&sidecars).into_iter().flatten().count();
    println!(
        "dogfood: {converted} document sidecar(s) ready in {}",
        sidecars.display()
    );
    // Last, so that everything before it — the clone, the wiring, the primed
    // documents — is the same work in both arms and the levers are the only
    // thing that differs.
    if arm == Some(Arm::Off) {
        let policy = crate::dogfood_compare::write_levers_off(scratch)?;
        println!("dogfood: every read lever off in {}", policy.display());
    }
    // The baseline, taken here rather than beside the refusal above: everything
    // the preparation books — the four primed `doc read` calls above most of
    // all — belongs to the preparation, not to the first session.
    let primed = parse_savings(&must(
        rift,
        &["savings".to_owned(), "--json".to_owned()],
        scratch,
    )?)?;
    Ok(Prepared::new(theirs, primed))
}

/// Play every scripted session for one client.
fn run_client(
    root: &Path,
    rift: &Path,
    client: Client,
    out: &Path,
    sessions: &[Session],
    seen: &[(String, String)],
    arm: Option<Arm>,
) -> Result<ClientRun, String> {
    // Without an arm the layout is what it has always been; each arm of a
    // calibration gets a checkout and a log directory of its own beneath it.
    let base = arm.map_or_else(
        || out.join(client.slug()),
        |arm| out.join(client.slug()).join(arm.label()),
    );
    let scratch = base.join("repo");
    let log_dir = base.join("logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("dogfood: {}: {e}", log_dir.display()))?;
    let prepared = prepare(root, rift, client, &scratch, seen, arm)?;
    let program = resolve_program(client.program())?;
    println!(
        "dogfood: {}{} — {} via {}, scratch {}",
        client.slug(),
        arm.map_or_else(String::new, |a| format!(" ({} levers)", a.label())),
        sessions.len(),
        program.display(),
        scratch.display()
    );
    let mut played = Vec::with_capacity(sessions.len());
    // The reading a session is measured against is the one taken before it, so
    // each session carries what it alone moved rather than the family's
    // running total. The first of them is the end of the preparation, so
    // session one carries no part of the priming.
    let mut before = prepared.baseline().clone();
    for session in sessions {
        let mut outcome = play(client, &program, rift, session, &scratch, &log_dir);
        outcome.ledger_lines = ledger_lines(&scratch);
        if let Ok(text) = must(rift, &["savings".to_owned(), "--json".to_owned()], &scratch) {
            let _ = std::fs::write(
                log_dir.join(format!("{}.savings.json", outcome.name)),
                &text,
            );
            outcome.savings = parse_savings(&text).ok();
            if let Some(after) = &outcome.savings {
                outcome.deltas = SavingsDeltas::between(&before, after);
                before = after.clone();
            }
        }
        println!(
            "  {} {} in {}s — share {}",
            outcome.name,
            outcome.status.label(),
            outcome.seconds,
            fmt_share(outcome.savings.as_ref().and_then(|s| s.share_pct))
        );
        played.push(outcome);
    }
    // A run where no session captured a document falls back to the reading the
    // preparation ended on, which is the family as the sessions found it.
    let final_savings = played
        .iter()
        .rev()
        .find_map(|s| s.savings.clone())
        .or_else(|| Some(prepared.baseline().clone()));
    // One reread report per client, at the end: it is the surface that groups
    // the audit relation by verb, and it writes only to the scratch family's
    // own projection.
    let kinds = capture(
        rift,
        &[
            "memory".to_owned(),
            "reread-report".to_owned(),
            "--json".to_owned(),
        ],
        &scratch,
    )
    .ok()
    .and_then(|(_, stdout, _)| {
        let _ = std::fs::write(log_dir.join("reread-report.json"), &stdout);
        parse_kinds(&stdout).ok()
    })
    .unwrap_or_default();
    Ok(ClientRun {
        client,
        scratch,
        // The family this checkout minted, as the refusal above read it.
        recall_db: prepared.minted().recall_db.clone(),
        projection_db: prepared.minted().projection_db.clone(),
        sessions: played,
        final_savings,
        kinds,
    })
}

/// The developer's global Codex configuration.
///
/// Codex writes a `trust_level` row into this file for a project directory it
/// has run in — the harness never asks for that, and it is not the harness's
/// file to change. So it is copied aside with its timestamp before a client
/// runs and put back byte for byte afterwards, which is what the archived
/// verification of this behaviour did.
fn codex_config() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".codex").join("config.toml"))
        .filter(|p| p.is_file())
}

/// A file copied aside so it can be put back exactly as it was.
struct Untouched {
    /// The file that must end the run as it began.
    path: PathBuf,
    /// The copy, made with its modification time preserved.
    backup: PathBuf,
}

impl Untouched {
    /// Copy `path` aside into `out`, timestamp and all.
    fn snapshot(path: PathBuf, out: &Path) -> Option<Self> {
        let backup = out.join("codex-config.toml.bak");
        let copied = Command::new("/bin/cp")
            .arg("-p")
            .arg(&path)
            .arg(&backup)
            .status()
            .is_ok_and(|s| s.success());
        copied.then_some(Self { path, backup })
    }

    /// Put the file back if anything changed it. Returns whether it had to.
    fn restore(&self) -> bool {
        let (Ok(now), Ok(then)) = (std::fs::read(&self.path), std::fs::read(&self.backup)) else {
            return false;
        };
        if now == then {
            return false;
        }
        let _ = std::fs::write(&self.path, &then);
        // `-r` copies the reference file's timestamps, so the file ends the
        // run with the modification time it started with as well as the bytes.
        let _ = Command::new("/usr/bin/touch")
            .arg("-r")
            .arg(&self.backup)
            .arg(&self.path)
            .status();
        true
    }
}

/// Delete a client's scratch checkout and sweep the registry row it created.
fn tidy(rift: &Path, root: &Path, run: &ClientRun) {
    if std::fs::remove_dir_all(&run.scratch).is_err() {
        println!(
            "dogfood: could not remove {} — remove it by hand",
            run.scratch.display()
        );
        return;
    }
    // The registry is append-only; `prune` appends one superseding removed
    // event per row whose path is gone, which is exactly what the scratch
    // checkout's row now is.
    match capture(rift, &["projects".to_owned(), "prune".to_owned()], root) {
        Ok((Some(0), _, _)) => {}
        _ => println!("dogfood: `rift projects prune` did not run — sweep the row by hand"),
    }
}

/// What a run played: the readings, the calibrations, and every checkout that
/// has to be kept or swept afterwards.
struct Played {
    /// The runs the single-arm table and document draw — the on arm, when a
    /// client played two.
    readings: Vec<ClientRun>,
    /// The clients that played both arms.
    pairs: Vec<CompareRun>,
    /// Checkouts that are neither: an arm whose twin never ran. It exists, so
    /// it is kept or swept with the rest — and it is deliberately **not** a
    /// reading, because half a calibration printed as one would be a share
    /// measured with the levers off.
    stray: Vec<ClientRun>,
}

impl Played {
    /// Every checkout this run made.
    fn checkouts(&self) -> impl Iterator<Item = &ClientRun> {
        self.readings
            .iter()
            .chain(self.pairs.iter().map(|p| &p.off))
            .chain(&self.stray)
    }
}

/// Play every client the run was asked for, one arm or two.
///
/// A client whose preparation failed is reported and skipped: one client that
/// cannot run is not a reason to lose the readings of the ones that can.
fn play_clients(
    root: &Path,
    rift: &Path,
    out: &Path,
    sessions: &[Session],
    opts: &Options,
    mine: &SavingsSample,
) -> Played {
    // The families this run is already measuring, the developer's first. Each
    // arm's checkout is refused if it resolves to one of them.
    let mut seen = vec![(mine.recall_db.clone(), mine.projection_db.clone())];
    let mut out_played = Played {
        readings: Vec::with_capacity(opts.clients.len()),
        pairs: Vec::new(),
        stray: Vec::new(),
    };
    for client in &opts.clients {
        let arms: Vec<Option<Arm>> = if opts.compare {
            ARMS.iter().map(|a| Some(*a)).collect()
        } else {
            vec![None]
        };
        let mut played: Vec<ClientRun> = Vec::with_capacity(arms.len());
        for arm in arms {
            match run_client(root, rift, *client, out, sessions, &seen, arm) {
                Ok(run) => {
                    seen.push((run.recall_db.clone(), run.projection_db.clone()));
                    played.push(run);
                }
                Err(e) => {
                    println!(
                        "dogfood: {}{} could not be prepared — {e}",
                        client.slug(),
                        arm.map_or_else(String::new, |a| format!(" ({} levers)", a.label()))
                    );
                    break;
                }
            }
        }
        match (opts.compare, played.as_slice()) {
            (true, [off, on]) => {
                out_played.readings.push(on.clone());
                out_played.pairs.push(CompareRun {
                    client: *client,
                    off: off.clone(),
                    on: on.clone(),
                });
            }
            // Half a calibration is not a reading; it is a checkout to sweep.
            (true, _) => out_played.stray.extend(played),
            (false, _) => out_played.readings.extend(played),
        }
    }
    out_played
}

/// Print the calibration and write it beside the single-arm report.
///
/// # Errors
/// The document or the table could not be written.
fn write_calibration(out: &Path, pairs: &[CompareRun], stamp: &str) -> Result<(), String> {
    let calibration = render_compare_table(pairs);
    println!("\n{calibration}");
    let path = out.join("compare.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&compare_json(pairs, stamp)).unwrap_or_default(),
    )
    .map_err(|e| format!("dogfood: {}: {e}", path.display()))?;
    std::fs::write(out.join("compare.txt"), &calibration)
        .map_err(|e| format!("dogfood: writing the calibration: {e}"))?;
    println!("dogfood: compare {}", path.display());
    Ok(())
}

/// Play the scripted sessions and print the reading.
///
/// # Errors
/// A harness failure: running under CI, a missing session file, a scratch
/// checkout that resolved to the developer's own family, or a `rift` verb that
/// refused. A client that cannot run is recorded, not raised.
pub fn run(root: &Path, opts: &Options) -> Result<(), String> {
    if std::env::var("CI").is_ok_and(|v| !v.is_empty()) {
        return Err(
            "dogfood: this command drives real clients and spends real API quota; \
             it does not run under CI"
                .to_owned(),
        );
    }
    let all = load_sessions(&root.join(SESSIONS_DIR))?;
    let sessions: Vec<Session> = all.into_iter().take(opts.sessions).collect();
    let stamp = utc_stamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    );
    let out = match &opts.out {
        Some(dir) => dir.clone(),
        None => PathBuf::from(std::env::var("HOME").map_err(|_| "dogfood: no HOME".to_owned())?)
            .join("rift-dogfood")
            .join(&stamp),
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("dogfood: {}: {e}", out.display()))?;
    let rift = release_binary(root)?;
    let mine = parse_savings(&must(
        &rift,
        &["savings".to_owned(), "--json".to_owned()],
        root,
    )?)?;
    println!(
        "dogfood: {} client(s) x {} scripted session(s){} through the clients on this machine.\n\
         This SPENDS REAL API QUOTA on each one. Output: {}\n\
         The developer's own family ({}) is not touched.",
        opts.clients.len(),
        sessions.len(),
        if opts.compare {
            " x 2 arms (levers off, then on)"
        } else {
            ""
        },
        out.display(),
        mine.recall_db
    );
    let untouched = codex_config().and_then(|p| Untouched::snapshot(p, &out));
    let played = play_clients(root, &rift, &out, &sessions, opts, &mine);
    let (runs, pairs) = (&played.readings, &played.pairs);
    if let Some(untouched) = &untouched {
        if untouched.restore() {
            println!(
                "dogfood: {} was changed by a client and has been put back as it was",
                untouched.path.display()
            );
        }
    }
    let table = render_table(runs);
    println!("\n{table}");
    let report = report_json(runs, &stamp);
    let json_path = out.join("dogfood.json");
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    )
    .map_err(|e| format!("dogfood: {}: {e}", json_path.display()))?;
    std::fs::write(out.join("dogfood.txt"), &table)
        .map_err(|e| format!("dogfood: writing the table: {e}"))?;
    println!("dogfood: report {}", json_path.display());
    if opts.compare {
        write_calibration(&out, pairs, &stamp)?;
    }
    println!("dogfood: logs   {}", out.display());
    // Both arms are kept, or both are swept: an arm left behind would be a
    // registry row and a family nobody meant to keep.
    if opts.keep {
        for run in played.checkouts() {
            println!(
                "dogfood: kept {} (its registry row is still there)",
                run.scratch.display()
            );
        }
    } else {
        for run in played.checkouts() {
            tidy(&rift, root, run);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// A `rift savings --json` document captured from a live family, with the
    /// home directory generalised. The parser is held to real output rather
    /// than to a shape somebody remembered.
    const SAVINGS_SAMPLE: &str = include_str!("../dogfood/testdata/savings-sample.json");
    /// The `levers` half of a live `rift memory reread-report --json`.
    const REREAD_SAMPLE: &str = include_str!("../dogfood/testdata/reread-sample.json");

    fn sample_run(client: Client, savings: SavingsSample, kinds: Vec<LeverKind>) -> ClientRun {
        ClientRun {
            client,
            scratch: PathBuf::from("/scratch").join(client.slug()),
            recall_db: savings.recall_db.clone(),
            projection_db: savings.projection_db.clone(),
            sessions: vec![
                SessionOutcome {
                    number: 1,
                    name: "one".to_owned(),
                    status: Status::Completed,
                    exit_code: Some(0),
                    seconds: 12,
                    savings: Some(savings.clone()),
                    deltas: SavingsDeltas::default(),
                    client_reported: None,
                    ledger_lines: 4,
                    retries: RetryCounts {
                        denies: 2,
                        retries: 1,
                        observed: true,
                    },
                    error: None,
                },
                SessionOutcome {
                    number: 2,
                    name: "two".to_owned(),
                    status: Status::Unavailable,
                    exit_code: Some(1),
                    seconds: 3,
                    savings: None,
                    deltas: SavingsDeltas::default(),
                    client_reported: None,
                    ledger_lines: 4,
                    retries: RetryCounts::default(),
                    error: Some("Individual quota reached".to_owned()),
                },
            ],
            final_savings: Some(savings),
            kinds,
        }
    }

    // --- the scripted sessions ---

    /// Every session that ships parses, and the set is the one the command
    /// promises: ten, numbered from one, each with an intent of its own.
    #[test]
    fn the_shipped_sessions_load() {
        let sessions = load_sessions(&workspace_root().join(SESSIONS_DIR)).unwrap();
        assert_eq!(sessions.len(), 10, "ten scripted sessions ship");
        for (at, s) in sessions.iter().enumerate() {
            assert_eq!(s.number as usize, at + 1, "{} is out of order", s.name);
            assert!(!s.intent.is_empty());
            assert!(s.prompt.len() > 40, "{} has a stub prompt", s.name);
        }
    }

    /// The prompts are ordinary work. Two things they must never do: ask the
    /// client to write history, or tell it what is being measured — a client
    /// told it is in a test stops behaving like a client.
    #[test]
    fn the_prompts_ask_for_ordinary_work_only() {
        let sessions = load_sessions(&workspace_root().join(SESSIONS_DIR)).unwrap();
        for s in &sessions {
            let lower = s.prompt.to_lowercase();
            for banned in ["commit", "push", " hook", "measure", "token"] {
                assert!(
                    !lower.contains(banned),
                    "{}: the prompt says `{banned}`",
                    s.name
                );
            }
        }
    }

    /// The whole shape of a session file, and each way it can be wrong.
    #[test]
    fn a_session_file_states_its_intent_and_its_prompt() {
        let ok = parse_session("03-a-name.md", "<!-- intent: read a file -->\nRead x.\n").unwrap();
        assert_eq!(ok.number, 3);
        assert_eq!(ok.name, "03-a-name");
        assert_eq!(ok.intent, "read a file");
        assert_eq!(ok.prompt, "Read x.");
        assert!(parse_session("03-a-name.txt", "x").is_err(), "not markdown");
        assert!(parse_session("x-name.md", "x").is_err(), "no ordinal");
        assert!(parse_session("03name.md", "x").is_err(), "no separator");
        assert!(
            parse_session("03-a.md", "Read x.\n").is_err(),
            "no intent header"
        );
        assert!(
            parse_session("03-a.md", "<!-- intent: x -->\n\n").is_err(),
            "no prompt"
        );
        assert!(
            parse_session("03-a.md", "<!-- intent:  -->\nRead x.").is_err(),
            "empty intent"
        );
    }

    // --- reading what Rift recorded ---

    /// The parser against real output, field for field.
    #[test]
    fn a_captured_savings_document_parses() {
        let s = parse_savings(SAVINGS_SAMPLE).unwrap();
        assert_eq!(s.tokens_avoided, 479);
        assert_eq!(s.tokens_returned, 101);
        assert_eq!(s.read_tokens, 472_420);
        assert_eq!(s.honesty, 378);
        assert_eq!(s.sessions_with_reads, 1);
        // The captured document predates the prevented figure, and an absent
        // figure is absent rather than zero.
        assert_eq!(s.tokens_prevented, None);
        assert!(s.note.is_none());
        assert!(s.share_pct.is_some_and(|v| (v - 0.101_290_1).abs() < 1e-6));
        assert!(s.window.starts_with("since the boundary marker"));
        assert!(s.recall_db.ends_with("recall.db"), "{}", s.recall_db);
        assert!(s.projection_db.ends_with("projection.db"));
    }

    /// A document without the block the figures come from is a refusal, not a
    /// row of zeros: a table that silently reads nothing is the one failure
    /// mode a savings harness must not have.
    #[test]
    fn savings_output_that_is_not_the_command_is_refused() {
        assert!(parse_savings("not json").is_err());
        assert!(parse_savings("{\"window\":{}}").is_err());
    }

    /// A family that has served nothing has no figures, and says so.
    #[test]
    fn an_unmeasured_family_carries_its_note() {
        let s = parse_savings(
            "{\"note\":\"nothing has been captured yet\",\"overall\":{\"share_pct\":null}}",
        )
        .unwrap();
        assert_eq!(s.note.as_deref(), Some("nothing has been captured yet"));
        assert_eq!(s.share_pct, None);
        assert_eq!(fmt_share(s.share_pct), "n/a");
    }

    /// The verbs group into the three levers the table names, and a verb this
    /// table has never seen lands under `other` rather than in a lever it does
    /// not belong to.
    #[test]
    fn verbs_group_by_the_lever_that_produced_them() {
        assert_eq!(
            classify_verb("memd.read_intercepted"),
            DecisionKind::Intercept
        );
        assert_eq!(
            classify_verb("memd.recovery_injected"),
            DecisionKind::Intercept
        );
        assert_eq!(
            classify_verb("memd.manifest_served"),
            DecisionKind::Intercept
        );
        assert_eq!(classify_verb("memd.known_state"), DecisionKind::Intercept);
        assert_eq!(classify_verb("memd.doc_rewritten"), DecisionKind::Document);
        assert_eq!(classify_verb("memd.doc_deferred"), DecisionKind::Document);
        assert_eq!(classify_verb("memd.search_answered"), DecisionKind::Search);
        assert_eq!(classify_verb("memd.grep_capped"), DecisionKind::Search);
        assert_eq!(classify_verb("memd.pack_built"), DecisionKind::Other);
    }

    /// The reread report's kinds, out of live output, and the tally the table
    /// draws from them.
    #[test]
    fn a_captured_reread_report_yields_its_kinds() {
        let kinds = parse_kinds(REREAD_SAMPLE).unwrap();
        assert_eq!(kinds.len(), 4);
        let intercepted = kinds.iter().find(|k| k.verb == "memd.read_intercepted");
        assert_eq!(intercepted.map(|k| k.tokens_avoided), Some(14_467));
        let tally = tally_kinds(&kinds);
        assert_eq!(tally.get(&DecisionKind::Intercept), Some(&42));
        assert_eq!(tally.get(&DecisionKind::Document), None);
        assert!(parse_kinds("{}").unwrap().is_empty(), "no levers, no kinds");
        assert!(parse_kinds("nope").is_err());
    }

    // --- what the client's own log exposes ---

    /// A decline the model was shown, then the same call again: one deny, one
    /// retry. A second file that was never declined is not counted.
    #[test]
    fn a_repeated_call_after_a_decline_is_a_retry() {
        let log = "{\"tool_use\":{\"file_path\":\"a.rs\"}}\n\
                   {\"result\":\"[rift] Read skipped\",\"file_path\":\"a.rs\"}\n\
                   {\"tool_use\":{\"file_path\":\"a.rs\"}}\n\
                   {\"tool_use\":{\"file_path\":\"b.rs\"}}\n";
        let r = retries_after_deny(log);
        assert!(r.observed);
        assert_eq!(r.denies, 1);
        assert_eq!(r.retries, 1);
    }

    /// A decline nobody acted on is a deny with no retry, and a log that names
    /// no target at all reports nothing rather than a zero it cannot defend.
    #[test]
    fn a_log_that_names_no_target_reports_nothing() {
        let declined = retries_after_deny(
            "{\"result\":\"[rift] Read redirected\",\"command\":\"cat spec.docx\"}\n",
        );
        assert_eq!((declined.denies, declined.retries), (1, 0));
        let silent = retries_after_deny("{\"type\":\"assistant\"}\nplain text\n");
        assert!(!silent.observed);
        assert_eq!((silent.denies, silent.retries), (0, 0));
    }

    /// The scan reads the fields the three clients actually name their targets
    /// in, and reads an escaped value without ending it early.
    #[test]
    fn the_scan_reads_every_clients_target_field() {
        assert_eq!(targets_on("{\"file_path\":\"a.rs\"}"), vec!["a.rs"]);
        assert_eq!(
            targets_on("{\"AbsolutePath\":\"/x/b.rs\"}"),
            vec!["/x/b.rs"]
        );
        assert_eq!(targets_on("{\"command\":\"cat c.md\"}"), vec!["cat c.md"]);
        assert_eq!(targets_on("{\"pattern\":\"needle\"}"), vec!["needle"]);
        assert_eq!(targets_on("{\"file_path\":\"a\\\"b\"}"), vec!["a\"b"]);
        assert!(targets_on("{\"other\":\"a.rs\"}").is_empty());
    }

    /// A client that has run out of quota, or is not logged in, is a session
    /// the reading is short by — not a measured zero.
    #[test]
    fn a_client_that_cannot_run_is_named_as_unavailable() {
        assert!(unavailability("Individual quota reached. Resets in 56h.")
            .is_some_and(|r| r.contains("quota reached")));
        assert!(unavailability("error: rate limit exceeded").is_some());
        // The model's own shell command failing is ordinary work inside a
        // session, not a client that could not run — this cost a whole run's
        // Codex leg the first time the harness ran.
        assert!(
            unavailability("zsh:1: command not found: rift").is_none(),
            "a shell command the model ran is not the client refusing"
        );
        assert!(unavailability("{\"type\":\"result\",\"subtype\":\"success\"}").is_none());
    }

    // --- the table and the JSON ---

    /// The table prints a row per client and one overall row, and the overall
    /// share is computed over the summed tokens rather than averaged across
    /// clients — an average of shares would weight a client with three reads
    /// the same as one with three thousand.
    #[test]
    fn the_table_carries_every_client_and_one_overall_row() {
        let a = SavingsSample {
            share_pct: Some(20.0),
            tokens_avoided: 200,
            tokens_prevented: Some(40),
            tokens_returned: 50,
            read_tokens: 800,
            honesty: 150,
            sessions_with_reads: 2,
            window: "since the reset".to_owned(),
            projection_db: "/a/projection.db".to_owned(),
            recall_db: "/a/recall.db".to_owned(),
            note: None,
        };
        let b = SavingsSample {
            share_pct: Some(10.0),
            tokens_avoided: 100,
            read_tokens: 900,
            recall_db: "/b/recall.db".to_owned(),
            projection_db: "/b/projection.db".to_owned(),
            ..a.clone()
        };
        let runs = vec![
            sample_run(
                Client::ClaudeCode,
                a,
                vec![LeverKind {
                    verb: "memd.read_intercepted".to_owned(),
                    rows: 3,
                    tokens_returned: 10,
                    tokens_avoided: 200,
                }],
            ),
            sample_run(
                Client::Codex,
                b,
                vec![LeverKind {
                    verb: "memd.doc_rewritten".to_owned(),
                    rows: 2,
                    tokens_returned: 5,
                    tokens_avoided: 100,
                }],
            ),
        ];
        let table = render_table(&runs);
        assert!(table.contains("claude-code"), "{table}");
        assert!(table.contains("codex"), "{table}");
        // 300 avoided over 300 + 1700 read.
        assert!(table.contains("15.00%"), "{table}");
        assert!(
            table.contains("1/2"),
            "one of two sessions completed\n{table}"
        );
        assert!(table.contains("UNAVAILABLE"), "{table}");
        assert!(table.contains("Individual quota reached"), "{table}");
        // The kind columns come from the verbs, not from the session count.
        assert!(table.contains("intercept"), "{table}");
        assert!(table.contains("1/2"), "{table}");
    }

    /// With nothing to report the table still draws, and says so where a
    /// reader would otherwise see a blank.
    #[test]
    fn an_empty_run_still_draws_a_table() {
        let table = render_table(&[]);
        assert!(table.contains("overall"));
        assert!(
            table.contains("n/a"),
            "no share is not a zero share\n{table}"
        );
        assert!(table.contains("(none)"));
    }

    /// The JSON carries the same figures as the table, plus the caveat the
    /// number cannot carry on its own.
    #[test]
    fn the_json_carries_the_figures_and_the_caveat() {
        let s = parse_savings(SAVINGS_SAMPLE).unwrap();
        let runs = vec![sample_run(
            Client::Antigravity,
            s,
            parse_kinds(REREAD_SAMPLE).unwrap(),
        )];
        let json = report_json(&runs, "20260908-120000");
        assert_eq!(json["kind"], "dogfood-reading");
        assert_eq!(json["stamp"], "20260908-120000");
        assert!(json["caveat"]
            .as_str()
            .is_some_and(|c| c.contains("not a gate")));
        let client = &json["clients"][0];
        assert_eq!(client["client"], "antigravity");
        assert_eq!(client["savings"]["tokens_avoided"], 479);
        assert_eq!(client["decisions_by_kind"]["intercept"], 42);
        assert_eq!(client["decisions_by_kind"]["document"], 0);
        assert_eq!(client["retry_after_deny"]["denies"], 2);
        assert_eq!(client["sessions_completed"], 1);
        assert_eq!(client["sessions"][1]["status"], "UNAVAILABLE");
        assert_eq!(client["sessions"][0]["ledger_lines"], 4);
    }

    /// A client whose log never exposed a decision reports that, rather than a
    /// retry rate of zero.
    #[test]
    fn an_unobserved_retry_rate_is_absent_from_the_json() {
        let mut run = sample_run(Client::Codex, SavingsSample::default(), Vec::new());
        for s in &mut run.sessions {
            s.retries = RetryCounts::default();
        }
        let json = report_json(&[run], "s");
        assert_eq!(json["clients"][0]["retry_after_deny"]["observed"], false);
    }

    // --- the command line ---

    /// Every flag, and each way the line can be wrong.
    #[test]
    fn the_arguments_parse() {
        let args: Vec<String> = [
            "--client",
            "all",
            "--sessions",
            "3",
            "--out",
            "/o",
            "--keep",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let opts = parse_args(&args).unwrap();
        assert_eq!(opts.clients, ALL_CLIENTS.to_vec());
        assert_eq!(opts.sessions, 3);
        assert_eq!(opts.out, Some(PathBuf::from("/o")));
        assert!(opts.keep);
        assert!(!opts.compare, "one arm unless the line asks for two");
        let both = parse_args(&[
            "--client".to_owned(),
            "codex".to_owned(),
            "--compare".to_owned(),
        ])
        .unwrap();
        assert!(both.compare);
        assert!(!both.keep, "the two flags are independent");
        let one = parse_args(&["--client".to_owned(), "codex".to_owned()]).unwrap();
        assert_eq!(one.clients, vec![Client::Codex]);
        assert_eq!(one.sessions, 10);
        assert!(!one.keep);
        assert!(parse_args(&[]).is_err(), "--client is required");
        assert!(
            parse_args(&["--compare".to_owned()]).is_err(),
            "still no client"
        );
        assert!(parse_args(&["--client".to_owned(), "vim".to_owned()]).is_err());
        assert!(parse_args(&["--client".to_owned()]).is_err(), "no value");
        assert!(parse_args(&["--wat".to_owned()]).is_err());
        assert!(parse_args(&[
            "--client".to_owned(),
            "codex".to_owned(),
            "--sessions".to_owned(),
            "0".to_owned()
        ])
        .is_err());
    }

    /// Neither arm may play in a family another arm — or the developer — is
    /// already measuring. Two arms sharing one ledger would answer the
    /// question the calibration exists to ask, by construction.
    #[test]
    fn the_two_arms_never_share_a_family() {
        let family = |tag: &str| SavingsSample {
            recall_db: format!("/scratch/{tag}/recall.db"),
            projection_db: format!("/scratch/{tag}/projection.db"),
            ..SavingsSample::default()
        };
        let mine = family("mine");
        let off = family("off");
        let on = family("on");
        let seen = |s: &[&SavingsSample]| -> Vec<(String, String)> {
            s.iter()
                .map(|v| (v.recall_db.clone(), v.projection_db.clone()))
                .collect()
        };
        // The off arm against the developer's family, then the on arm against
        // both — the order the run checks them in.
        assert!(refuse_shared_family(&off, &seen(&[&mine])).is_ok());
        assert!(refuse_shared_family(&on, &seen(&[&mine, &off])).is_ok());
        // The same recall ledger twice.
        let err = refuse_shared_family(&off, &seen(&[&mine, &off])).unwrap_err();
        assert!(err.contains("already being measured"), "{err}");
        assert!(err.contains("/scratch/off/recall.db"), "{err}");
        // A shared capture projection is the same refusal: either half is one
        // family's durable state.
        let half = SavingsSample {
            recall_db: "/scratch/elsewhere/recall.db".to_owned(),
            projection_db: mine.projection_db.clone(),
            ..SavingsSample::default()
        };
        assert!(refuse_shared_family(&half, &seen(&[&mine])).is_err());
        // And with nothing measured yet, nothing is refused.
        assert!(refuse_shared_family(&off, &[]).is_ok());
    }

    /// Each client is driven headlessly, with the least permission that lets
    /// it read, edit, glob and grep — and Codex is never handed the flag that
    /// writes a trust row into the developer's global configuration.
    #[test]
    fn each_client_is_driven_headlessly() {
        let repo = Path::new("/scratch/repo");
        let claude = Client::ClaudeCode.argv("hello", repo);
        assert!(claude.contains(&"-p".to_owned()));
        assert!(claude.contains(&"hello".to_owned()));
        assert!(claude.contains(&"project,local".to_owned()));
        assert!(claude.contains(&"acceptEdits".to_owned()));
        assert!(!claude.contains(&"--dangerously-skip-permissions".to_owned()));
        let codex = Client::Codex.argv("hello", repo);
        assert_eq!(codex.first().map(String::as_str), Some("exec"));
        assert!(codex.contains(&"--dangerously-bypass-hook-trust".to_owned()));
        assert!(codex.contains(&"--skip-git-repo-check".to_owned()));
        assert!(
            !codex.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()),
            "that flag persists a trust row in the developer's own client configuration"
        );
        let agy = Client::Antigravity.argv("hello", repo);
        assert!(
            agy.contains(&"--new-project".to_owned()),
            "a fresh conversation each time"
        );
        assert!(agy.contains(&"/scratch/repo".to_owned()));
    }

    /// The slugs are the ones `rift enable --agent` accepts, and `all` is all
    /// three.
    #[test]
    fn the_client_names_are_the_adapter_names() {
        assert_eq!(Client::parse("all").unwrap().len(), 3);
        assert_eq!(
            Client::parse("claude-code").unwrap(),
            vec![Client::ClaudeCode]
        );
        assert_eq!(
            Client::parse("antigravity").unwrap(),
            vec![Client::Antigravity]
        );
        assert!(
            Client::parse("gemini").is_err(),
            "only three can be enabled"
        );
        assert_eq!(Client::ClaudeCode.program(), "claude");
        assert_eq!(Client::Codex.program(), "codex");
        assert_eq!(Client::Antigravity.program(), "agy");
    }

    /// The run stamp, against instants whose civil date is known — including a
    /// leap day, which is where a hand-rolled calendar goes wrong.
    #[test]
    fn the_run_stamp_is_the_utc_instant() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_788_830_405), "20260908-012005");
        assert_eq!(utc_stamp(1_709_164_800), "20240229-000000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }

    /// The prevented part of the avoided figure is read where the reporting
    /// surface carries it, and stays absent where it does not — the two are
    /// different answers, and only one of them is a number.
    #[test]
    fn the_prevented_part_is_read_where_it_is_reported() {
        let with = parse_savings(
            "{\"overall\":{\"tokens_avoided\":800,\"tokens_prevented\":300,\"read_tokens\":9200}}",
        )
        .unwrap();
        assert_eq!(with.tokens_avoided, 800);
        assert_eq!(with.tokens_prevented, Some(300));
        let without =
            parse_savings("{\"overall\":{\"tokens_avoided\":800,\"read_tokens\":9200}}").unwrap();
        assert_eq!(without.tokens_prevented, None);
    }

    /// A session carries what it alone moved, signed on every axis, and the
    /// prevented axis stays absent unless both readings carried it.
    #[test]
    fn a_session_carries_the_move_it_made() {
        let before = SavingsSample {
            read_tokens: 1_000,
            tokens_avoided: 100,
            tokens_prevented: Some(20),
            tokens_returned: 10,
            ..SavingsSample::default()
        };
        let after = SavingsSample {
            read_tokens: 4_000,
            tokens_avoided: 900,
            tokens_prevented: Some(300),
            tokens_returned: 25,
            ..SavingsSample::default()
        };
        let d = SavingsDeltas::between(&before, &after);
        assert_eq!(d.read_tokens, 3_000);
        assert_eq!(d.tokens_avoided, 800);
        assert_eq!(d.tokens_prevented, Some(280));
        assert_eq!(d.tokens_returned, 15);
        // A correction that lowers a counter is reported as the negative it
        // is; a clamp here would hide the one case worth finding.
        let back = SavingsDeltas::between(&after, &before);
        assert_eq!(back.read_tokens, -3_000);
        assert_eq!(back.tokens_avoided, -800);
        // One side without the figure is not a delta of zero.
        let silent = SavingsSample {
            tokens_prevented: None,
            ..after.clone()
        };
        assert_eq!(
            SavingsDeltas::between(&before, &silent).tokens_prevented,
            None
        );
    }

    /// The preparation primes the two documents by hand, and that priming
    /// books rows of its own. The baseline a session is measured against is
    /// therefore the reading taken *after* it, so the first session's delta is
    /// the first session's own work and not four `doc read` calls the harness
    /// made.
    #[test]
    fn the_baseline_is_sampled_after_the_priming() {
        // A fresh family: `init` has run and nothing has been booked.
        let minted = SavingsSample {
            recall_db: "/scratch/recall.db".to_owned(),
            projection_db: "/scratch/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        // The same family at the end of the preparation, carrying the shape a
        // live run reported for the priming alone: four document decisions.
        let primed = SavingsSample {
            tokens_avoided: 718,
            tokens_returned: 494,
            ..minted.clone()
        };
        let prepared = Prepared::new(minted.clone(), primed.clone());
        assert_eq!(prepared.baseline(), &primed, "the later reading");
        assert_eq!(prepared.minted(), &minted, "and the earlier one is kept");
        // What the family reads after the first session: the priming, plus the
        // session's own work.
        let after_one = SavingsSample {
            tokens_avoided: 718 + 240,
            tokens_returned: 494 + 130,
            read_tokens: 3_000,
            ..minted.clone()
        };
        let d = SavingsDeltas::between(prepared.baseline(), &after_one);
        assert_eq!(d.tokens_avoided, 240, "the session's own avoided tokens");
        assert_eq!(d.tokens_returned, 130, "and its own returned tokens");
        assert_eq!(d.read_tokens, 3_000);
        // Measured against the earlier reading it would absorb the priming,
        // which is the reading this baseline exists to avoid.
        let absorbed = SavingsDeltas::between(prepared.minted(), &after_one);
        assert_eq!(absorbed.tokens_avoided, 958);
        assert_eq!(absorbed.tokens_returned, 624);
    }

    /// The shared-family refusal keeps reading the first sample: it is taken
    /// while the checkout is still untouched, and a clone that resolved to a
    /// family already being measured has to be refused before the preparation
    /// enables anything or writes a row.
    #[test]
    fn the_refusal_still_reads_the_first_sample() {
        let minted = SavingsSample {
            recall_db: "/theirs/recall.db".to_owned(),
            projection_db: "/theirs/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        // Distinguishable only so this test can say which reading was
        // consulted; one preparation's two readings name one family.
        let primed = SavingsSample {
            recall_db: "/elsewhere/recall.db".to_owned(),
            projection_db: "/elsewhere/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        let prepared = Prepared::new(minted, primed);
        let seen = vec![(
            "/theirs/recall.db".to_owned(),
            "/theirs/projection.db".to_owned(),
        )];
        let refused = refuse_shared_family(prepared.minted(), &seen);
        assert!(refused.is_err(), "the family the refusal was made against");
        assert!(
            refused.unwrap_err().contains("/theirs/recall.db"),
            "and it names that family"
        );
        assert!(
            refuse_shared_family(prepared.baseline(), &seen).is_ok(),
            "the later reading names another family — refusing on it would let \
             a shared family through"
        );
    }

    /// The single-arm table and document are what they were before the
    /// calibration existed: the figures it collects are carried on the same
    /// rows, and none of them reaches this output.
    #[test]
    fn without_compare_the_table_is_unchanged() {
        let sample = parse_savings(SAVINGS_SAMPLE).unwrap();
        let plain = sample_run(
            Client::Codex,
            sample.clone(),
            parse_kinds(REREAD_SAMPLE).unwrap(),
        );
        let mut carrying = plain.clone();
        for s in &mut carrying.sessions {
            s.deltas = SavingsDeltas {
                read_tokens: 4_000,
                tokens_avoided: 900,
                tokens_prevented: Some(120),
                tokens_returned: 30,
            };
            s.client_reported = Some(76_667);
        }
        assert_eq!(
            render_table(std::slice::from_ref(&carrying)),
            render_table(std::slice::from_ref(&plain)),
            "the calibration's figures do not reach the single-arm table"
        );
        assert_eq!(
            report_json(std::slice::from_ref(&carrying), "20260909-000000"),
            report_json(std::slice::from_ref(&plain), "20260909-000000"),
            "nor its document"
        );
    }

    /// The documents the scripted sessions read are copied out of the
    /// converter's own corpus, and both are still there.
    #[test]
    fn the_documents_the_sessions_read_still_ship() {
        let root = workspace_root();
        for (from, to) in DOCUMENTS {
            assert!(root.join(from).is_file(), "{from} is gone");
            assert!(to.starts_with("docs/"), "{to} lands in the checkout");
        }
    }
}
