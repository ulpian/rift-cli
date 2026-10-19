//! The calibration arm of the scripted-session reading: the same sessions
//! played twice, once with the read levers on and once with every one of them
//! off, so the booked saving can be held against a removal somebody measured.
//!
//! The two arms are separate scratch clones with families of their own, so
//! neither arm can read or write the other's ledger. What they cannot be is
//! two runs of the same experiment: the models choose their own tool calls, so
//! a difference between the arms carries the levers *and* the day's
//! non-determinism. That is why the calibration is a reading a developer looks
//! at, never a figure the product quotes.

use std::path::{Path, PathBuf};

use toml_edit::{value, DocumentMut, Item, Table};

use crate::dogfood::{Client, ClientRun, Lever, Status};

/// Which side of the calibration a scratch checkout is playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Arm {
    /// Every read lever switched off: what the clients do without Rift acting
    /// on their calls at all.
    Off,
    /// The levers as they ship, which is what the single-arm reading measures.
    On,
    /// The levers as they ship, with the client's own read and search tools
    /// withheld so every read goes through its shell — the shape most of the
    /// field's traffic has. Played beside the calibration, never inside it.
    Shell,
}

/// Every arm, in the order the calibration plays and prints them.
pub const ARMS: [Arm; 2] = [Arm::Off, Arm::On];

impl Arm {
    /// The word the table, the document and the directory all use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Shell => "shell",
        }
    }

    /// The words a progress line puts after the client's name.
    ///
    /// The shell arm is not a lever setting — its levers are the on arm's —
    /// so it is named for what it withholds instead.
    #[must_use]
    pub fn heading(self) -> String {
        match self {
            Self::Off | Self::On => format!(" ({} levers)", self.label()),
            Self::Shell => " (on levers, shell reads only)".to_owned(),
        }
    }
}

/// The serving overlay, relative to a checkout.
const SERVE_POLICY: [&str; 3] = [".rift", "policies", "serve.toml"];

/// Rewrite a serving overlay so that every read lever is off.
///
/// Three tables carry a `mode`, and a per-family override *replaces* the table
/// it sits under rather than capping it — so an override left at `enforce`
/// under an `off` table is a lever that is still on. Every override is
/// therefore written off too, and the rest of the document is left exactly as
/// it was: the arm differs from its twin in the levers and in nothing else.
///
/// The refusal an unparsable policy would produce is deliberately not used as
/// the off switch. Refusing the file also denies exact retrieval and stops
/// injection, which would make the arm a measurement of two changes.
///
/// # Errors
/// A document that is not TOML, or one whose lever tables are not tables.
pub fn levers_off(text: &str) -> Result<String, String> {
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|e| format!("dogfood: the serving overlay is not TOML: {e}"))?
    };
    for lever in ["interception", "search", "doc"] {
        let table = table_mut(&mut doc, lever)?;
        set_str(table, "mode", Arm::Off.label());
    }
    let interception = table_mut(&mut doc, "interception")?;
    match interception.get_mut("agents") {
        Some(Item::Table(agents)) => {
            let keys: Vec<String> = agents.iter().map(|(k, _)| k.to_owned()).collect();
            for key in keys {
                set_str(agents, &key, Arm::Off.label());
            }
        }
        Some(Item::Value(toml_edit::Value::InlineTable(agents))) => {
            let keys: Vec<String> = agents.iter().map(|(k, _)| k.to_owned()).collect();
            for key in keys {
                agents.insert(&key, toml_edit::Value::from(Arm::Off.label()));
            }
        }
        _ => {}
    }
    Ok(doc.to_string())
}

/// Rewrite a serving overlay so that the named levers are at `enforce`.
///
/// Only the keys named move, each under `[interception]`, and the rest of the
/// document is left exactly as it was: the arm differs from a run without the
/// flag in those keys and in nothing else. `mode` is not touched — the family
/// and channel caps the resolver applies still hold, so a lever written here
/// acts only where a run without the flag would have observed it.
///
/// # Errors
/// A document that is not TOML, or one whose `[interception]` is not a table.
pub fn levers_enforced(text: &str, levers: &[Lever]) -> Result<String, String> {
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .map_err(|e| format!("dogfood: the serving overlay is not TOML: {e}"))?
    };
    let interception = table_mut(&mut doc, "interception")?;
    for lever in levers {
        set_str(interception, lever.key(), "enforce");
    }
    Ok(doc.to_string())
}

/// Switch every read lever off in one scratch checkout, and say where.
///
/// # Errors
/// What `write_overlay` refuses, or a document the lever rewrite could not
/// apply.
pub fn write_levers_off(scratch: &Path) -> Result<PathBuf, String> {
    write_overlay(scratch, levers_off)
}

/// Write the named levers at `enforce` in one scratch checkout, and say where.
///
/// # Errors
/// What `write_overlay` refuses, or a document the lever rewrite could not
/// apply.
pub fn write_levers_enforced(scratch: &Path, levers: &[Lever]) -> Result<PathBuf, String> {
    write_overlay(scratch, |text| levers_enforced(text, levers))
}

/// Rewrite one scratch checkout's serving overlay through `edit`.
///
/// The one place the harness writes a policy. The file is a repository-owned
/// path in a clone this harness made moments ago, so the write refuses a link
/// rather than following one, and lands through a sibling temporary file so a
/// crash cannot leave half a policy.
///
/// # Errors
/// A refused path, an unreadable or unwritable file, or an `edit` that
/// refused the document.
fn write_overlay(
    scratch: &Path,
    edit: impl FnOnce(&str) -> Result<String, String>,
) -> Result<PathBuf, String> {
    let mut path = scratch.to_path_buf();
    for part in SERVE_POLICY {
        path.push(part);
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file() {
            return Err(format!(
                "dogfood: {} is not a regular file — refusing to write an arm's policy",
                path.display()
            ));
        }
    }
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("dogfood: read {}: {e}", path.display())),
    };
    let next = edit(&current)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("dogfood: {}: {e}", dir.display()))?;
    }
    let temp = path.with_extension("toml.arm");
    std::fs::write(&temp, next.as_bytes())
        .map_err(|e| format!("dogfood: write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("dogfood: {}: {e}", path.display()))?;
    Ok(path)
}

/// The named table, created empty if the document has none.
fn table_mut<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table, String> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::Table(Table::new()));
    }
    doc.get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| format!("dogfood: [{key}] in the serving overlay is not a table"))
}

/// Set one string key, keeping whatever decoration the value already carried.
fn set_str(table: &mut Table, key: &str, text: &str) {
    if let Some(current) = table.get_mut(key).and_then(Item::as_value_mut) {
        let decor = current.decor().clone();
        *current = toml_edit::Value::from(text);
        *current.decor_mut() = decor;
    } else {
        table.insert(key, value(text));
    }
}

/// What the client's own event stream said one session cost it, in input
/// tokens — cached and uncached, because both are context the model was
/// charged for reading.
///
/// Each client is read at the one event that states a session total, and the
/// shapes are pinned to captured output rather than remembered:
///
/// * Claude Code closes a session with `{"type":"result", … "usage":{…}}`,
///   whose `input_tokens`, `cache_creation_input_tokens` and
///   `cache_read_input_tokens` already sum every assistant message. Summing
///   the per-message blocks instead would double-count, because the stream
///   repeats an assistant message's usage as the message streams.
/// * Codex states one `{"type":"turn.completed","usage":{…}}` per turn, whose
///   `input_tokens` includes its own `cached_input_tokens`, so the turns are
///   summed and the cached field is not added again.
/// * Antigravity closes with `{"event":"result","result":{"usage":{…}}}`,
///   which counts `cache_read_tokens` beside `input_tokens` rather than
///   inside it.
///
/// A stream that carries none of those — a session the time box cut, or a
/// client that never states a total — is `None`, which the table prints as
/// `-`. A zero would claim the session was free.
#[must_use]
pub fn client_reported_tokens(client: Client, log: &str) -> Option<u64> {
    let u = |v: Option<&serde_json::Value>, key: &str| -> u64 {
        v.and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let mut total: Option<u64> = None;
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match client {
            Client::ClaudeCode => {
                if v.get("type").and_then(serde_json::Value::as_str) != Some("result") {
                    continue;
                }
                let usage = v.get("usage");
                // The last one wins: the stream states this once, and a
                // resumed session would state the later total.
                total = Some(
                    u(usage, "input_tokens")
                        .saturating_add(u(usage, "cache_creation_input_tokens"))
                        .saturating_add(u(usage, "cache_read_input_tokens")),
                );
            }
            Client::Codex => {
                if v.get("type").and_then(serde_json::Value::as_str) != Some("turn.completed") {
                    continue;
                }
                let turn = u(v.get("usage"), "input_tokens");
                total = Some(total.unwrap_or(0).saturating_add(turn));
            }
            Client::Antigravity => {
                if v.get("event").and_then(serde_json::Value::as_str) != Some("result") {
                    continue;
                }
                let usage = v.pointer("/result/usage");
                total =
                    Some(u(usage, "input_tokens").saturating_add(u(usage, "cache_read_tokens")));
            }
        }
    }
    total
}

// --- the calibration itself ---------------------------------------------------

/// Both arms of one client's calibration.
#[derive(Clone, Debug)]
pub struct CompareRun {
    /// The client both arms were played through.
    pub client: Client,
    /// The arm with every read lever off.
    pub off: ClientRun,
    /// The arm as the levers ship.
    pub on: ClientRun,
}

/// One line of the calibration: a session, a client's total, or the run's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompareRow {
    /// The client's name, or `overall`.
    pub client: String,
    /// The session's name, or the word for a summed row.
    pub session: String,
    /// Read tokens the family booked with the levers off.
    pub read_off: i64,
    /// Read tokens the family booked with the levers on.
    pub read_on: i64,
    /// What the on arm booked as avoided.
    pub booked_avoided: i64,
    /// The prevented part of that, where the surface reported it.
    pub booked_prevented: Option<i64>,
    /// What the on arm booked as returned — the cost side of the honesty
    /// figure, carried in the document rather than the table.
    pub booked_returned: i64,
    /// What the client itself said the off arm cost it.
    pub reported_off: Option<u64>,
    /// What the client itself said the on arm cost it.
    pub reported_on: Option<u64>,
    /// Whether both arms of this row ran to completion. A row missing an arm
    /// is printed and marked, never dropped and never quietly counted.
    pub both_completed: bool,
}

impl CompareRow {
    /// The removal somebody measured: reads with the levers off, minus reads
    /// with them on.
    ///
    /// **Signed, and never clamped.** A negative measured removal says the
    /// arm with the levers on read *more*, which on two non-deterministic runs
    /// is an ordinary outcome and the single most important thing this table
    /// can tell a reader. Clamping it at zero would turn the one honest
    /// warning into a silent zero.
    #[must_use]
    pub const fn measured(&self) -> i64 {
        self.read_off - self.read_on
    }

    /// What was booked, over what was measured, as a percentage.
    ///
    /// `None` when the measured removal is zero or negative: a ratio over a
    /// non-positive denominator is not a calibration, and printing one would
    /// invent a reading out of noise.
    #[must_use]
    pub fn calibration(&self) -> Option<f64> {
        let measured = self.measured();
        (measured > 0).then(|| self.booked_avoided as f64 * 100.0 / measured as f64)
    }
}

/// The label a summed row carries in the session column.
pub(crate) const SUMMED: &str = "(all sessions)";

/// Add `b` into `a`, where an absent figure on either side stays absent — a
/// sum that treated silence as zero would understate the total it reports.
pub(crate) fn add_opt<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        _ => None,
    }
}

/// One row per scripted session, then the client's own summed row.
///
/// Sessions are paired by their ordinal, which is the only thing the two arms
/// share: they ran in different checkouts, against different families, at
/// different times.
#[must_use]
pub fn rows_for(run: &CompareRun) -> Vec<CompareRow> {
    let numbers = session_numbers(&run.off, &run.on);
    let mut rows = Vec::with_capacity(numbers.len() + 1);
    let find =
        |arm: &ClientRun, number: u32| arm.sessions.iter().find(|s| s.number == number).cloned();
    for number in numbers {
        let off = find(&run.off, number);
        let on = find(&run.on, number);
        let name = on
            .as_ref()
            .or(off.as_ref())
            .map_or_else(|| format!("{number:02}"), |s| s.name.clone());
        rows.push(CompareRow {
            client: run.client.slug().to_owned(),
            session: name,
            read_off: off.as_ref().map_or(0, |s| s.deltas.read_tokens),
            read_on: on.as_ref().map_or(0, |s| s.deltas.read_tokens),
            booked_avoided: on.as_ref().map_or(0, |s| s.deltas.tokens_avoided),
            booked_prevented: on.as_ref().and_then(|s| s.deltas.tokens_prevented),
            booked_returned: on.as_ref().map_or(0, |s| s.deltas.tokens_returned),
            reported_off: off.as_ref().and_then(|s| s.client_reported),
            reported_on: on.as_ref().and_then(|s| s.client_reported),
            both_completed: off.as_ref().is_some_and(|s| s.status == Status::Completed)
                && on.as_ref().is_some_and(|s| s.status == Status::Completed),
        });
    }
    let mut total = summed(&rows);
    run.client.slug().clone_into(&mut total.client);
    rows.push(total);
    rows
}

/// Every session ordinal either arm played, in order and once each.
pub(crate) fn session_numbers(a: &ClientRun, b: &ClientRun) -> Vec<u32> {
    let mut numbers: Vec<u32> = a
        .sessions
        .iter()
        .chain(&b.sessions)
        .map(|s| s.number)
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// Every client's rows, then one summed row over all of them.
#[must_use]
pub fn all_rows(runs: &[CompareRun]) -> Vec<CompareRow> {
    let mut rows: Vec<CompareRow> = runs.iter().flat_map(rows_for).collect();
    let per_session: Vec<CompareRow> = rows
        .iter()
        .filter(|r| r.session != SUMMED)
        .cloned()
        .collect();
    let mut overall = summed(&per_session);
    "overall".clone_into(&mut overall.client);
    rows.push(overall);
    rows
}

/// Sum a set of rows into one, keeping every figure signed.
fn summed(rows: &[CompareRow]) -> CompareRow {
    let mut out = CompareRow {
        session: SUMMED.to_owned(),
        both_completed: true,
        // An absent client figure poisons the sum rather than counting as
        // zero; the seed is therefore the identity, not `None`.
        booked_prevented: Some(0),
        reported_off: Some(0),
        reported_on: Some(0),
        ..CompareRow::default()
    };
    for r in rows {
        out.read_off += r.read_off;
        out.read_on += r.read_on;
        out.booked_avoided += r.booked_avoided;
        out.booked_returned += r.booked_returned;
        out.booked_prevented = add_opt(out.booked_prevented, r.booked_prevented);
        out.reported_off = add_opt(out.reported_off, r.reported_off);
        out.reported_on = add_opt(out.reported_on, r.reported_on);
        out.both_completed &= r.both_completed;
    }
    out
}

/// A figure the harness has, or the dash for one it does not.
#[must_use]
pub fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

/// The calibration as a percentage, or the dash for a reading there is no
/// denominator for.
#[must_use]
pub fn fmt_calibration(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| format!("{v:.2}%"))
}

/// Left-pad to `width` columns.
pub(crate) fn pad(label: &str, width: usize) -> String {
    let mut out = label.to_owned();
    for _ in label.chars().count()..width {
        out.push(' ');
    }
    out
}

/// The sentence the figure cannot carry on its own, printed under the table
/// and written into the document beside it.
pub const CAVEAT: &str = "the two arms are separate, non-deterministic model runs \
    against separate families: a difference between them carries the levers and the \
    day's tool choices together. A negative measured removal means the arm with the \
    levers on read more, and is reported as it stands, never clamped.";

/// The calibration table: one row per session per client, a summed row per
/// client, and one over the run.
#[must_use]
pub fn render_compare_table(runs: &[CompareRun]) -> String {
    let mut out = String::new();
    out.push_str(
        "calibration — the same scripted sessions with every read lever off, and as they ship\n",
    );
    out.push_str(&format!(
        "  {}{}{:>11}{:>11}{:>11}{:>11}{:>11}{:>13}{:>12}{:>12}\n",
        pad("client", 14),
        pad("session", 26),
        "read_off",
        "read_on",
        "measured",
        "avoided",
        "prevented",
        "calibration",
        "client_off",
        "client_on"
    ));
    let rows = all_rows(runs);
    let mut marked = false;
    for r in &rows {
        let mark = if r.both_completed { "" } else { " !" };
        marked |= !r.both_completed;
        out.push_str(&format!(
            "  {}{}{:>11}{:>11}{:>11}{:>11}{:>11}{:>13}{:>12}{:>12}\n",
            pad(&r.client, 14),
            pad(&format!("{}{mark}", r.session), 26),
            r.read_off,
            r.read_on,
            r.measured(),
            r.booked_avoided,
            fmt_opt(r.booked_prevented),
            fmt_calibration(r.calibration()),
            fmt_opt(r.reported_off),
            fmt_opt(r.reported_on),
        ));
    }
    if rows.is_empty() {
        out.push_str("  (nothing was played)\n");
    }
    if marked {
        out.push_str("  ! one arm of this row did not complete — it is not a measurement\n");
    }
    out.push('\n');
    for line in wrapped(CAVEAT, 76) {
        out.push_str(&format!("  {line}\n"));
    }
    out
}

/// The caveat, broken on spaces so a terminal does not have to.
pub(crate) fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The same facts as one JSON document, written beside the single-arm report.
#[must_use]
pub fn compare_json(runs: &[CompareRun], stamp: &str) -> serde_json::Value {
    let arm = |run: &ClientRun| {
        serde_json::json!({
            "scratch": run.scratch.display().to_string(),
            "recall_db": run.recall_db,
            "projection_db": run.projection_db,
            "sessions_completed": run.completed(),
            "sessions_played": run.sessions.len(),
        })
    };
    let row = |r: &CompareRow| {
        serde_json::json!({
            "client": r.client,
            "session": r.session,
            "read_off": r.read_off,
            "read_on": r.read_on,
            "measured": r.measured(),
            "booked_avoided": r.booked_avoided,
            "booked_prevented": r.booked_prevented,
            "booked_returned": r.booked_returned,
            "calibration_pct": r.calibration(),
            "client_reported_off": r.reported_off,
            "client_reported_on": r.reported_on,
            "both_arms_completed": r.both_completed,
        })
    };
    serde_json::json!({
        "kind": "dogfood-calibration",
        "definition": "read tokens with every read lever off, minus read tokens with them on, \
                       against what the levers booked as avoided over the same sessions",
        "caveat": CAVEAT,
        "stamp": stamp,
        "arms": ARMS.iter().map(|a| a.label()).collect::<Vec<_>>(),
        "clients": runs.iter().map(|r| serde_json::json!({
            "client": r.client.slug(),
            "off": arm(&r.off),
            "on": arm(&r.on),
        })).collect::<Vec<_>>(),
        "rows": all_rows(runs).iter().map(row).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dogfood::{SavingsDeltas, SavingsSample, SessionOutcome, Status};
    use std::path::PathBuf;

    /// One session's arm, as the calibration reads it: what it moved, what the
    /// client said it cost, and whether it finished.
    fn session(
        number: u32,
        read: i64,
        avoided: i64,
        prevented: Option<i64>,
        reported: Option<u64>,
        status: Status,
    ) -> SessionOutcome {
        SessionOutcome {
            number,
            name: format!("{number:02}-a-session"),
            status,
            exit_code: Some(0),
            seconds: 9,
            savings: Some(SavingsSample::default()),
            deltas: SavingsDeltas {
                read_tokens: read,
                tokens_avoided: avoided,
                tokens_prevented: prevented,
                tokens_returned: 25,
            },
            client_reported: reported,
            ledger_lines: 3,
            retries: crate::dogfood::RetryCounts::default(),
            error: None,
        }
    }

    /// One arm of one client, in a checkout and a family of its own.
    fn arm_run(client: Client, arm: Arm, sessions: Vec<SessionOutcome>) -> ClientRun {
        let root = PathBuf::from("/scratch")
            .join(client.slug())
            .join(arm.label());
        ClientRun {
            client,
            scratch: root.join("repo"),
            recall_db: root.join("recall.db").display().to_string(),
            projection_db: root.join("projection.db").display().to_string(),
            sessions,
            final_savings: Some(SavingsSample::default()),
            kinds: Vec::new(),
        }
    }

    /// One captured line per client, trimmed to the shape that carries the
    /// figure and with this machine's home directory generalised.
    const CLAUDE_CODE: &str = include_str!("../dogfood/testdata/usage-claude-code.jsonl");
    /// Codex's own turn total.
    const CODEX: &str = include_str!("../dogfood/testdata/usage-codex.jsonl");
    /// Antigravity's own session total.
    const ANTIGRAVITY: &str = include_str!("../dogfood/testdata/usage-antigravity.jsonl");

    /// Each client's own figure, out of that client's own captured stream —
    /// and, for the one that repeats a per-message usage block, the session
    /// total rather than the sum of the blocks.
    #[test]
    fn client_usage_is_read_from_each_clients_stream() {
        // 3273 input + 30426 cache creation + 42968 cache read. The assistant
        // line in the same fixture carries a usage block of its own, which is
        // part of that total and must not be added to it.
        assert_eq!(
            client_reported_tokens(Client::ClaudeCode, CLAUDE_CODE),
            Some(76_667)
        );
        // 205468 input, which already contains the 164224 cached.
        assert_eq!(client_reported_tokens(Client::Codex, CODEX), Some(205_468));
        // 41603 input + 44900 cache read, which this client counts beside it.
        assert_eq!(
            client_reported_tokens(Client::Antigravity, ANTIGRAVITY),
            Some(86_503)
        );
        // A stream is read for its own client's shape and no other's.
        assert_eq!(client_reported_tokens(Client::Codex, CLAUDE_CODE), None);
        assert_eq!(client_reported_tokens(Client::ClaudeCode, CODEX), None);
        // Nothing to read is absent, never zero.
        assert_eq!(client_reported_tokens(Client::ClaudeCode, ""), None);
        assert_eq!(
            client_reported_tokens(Client::Antigravity, "not json\n"),
            None
        );
    }

    /// The off arm's policy is read back through the loader that actually
    /// reads it, and every lever it can pull is off: the three tables, and the
    /// per-family override that would otherwise replace the table it sits
    /// under. Everything else in the document survives, because the arms must
    /// differ in the levers and in nothing else.
    #[test]
    fn the_off_arm_writes_every_lever_off() {
        use rift_recall_policy::{Mode, ServePolicy};

        let scratch = tempfile::tempdir().unwrap();
        let rift_dir = scratch.path().join(".rift");
        std::fs::create_dir_all(rift_dir.join("policies")).unwrap();
        std::fs::write(
            rift_dir.join("policies").join("serve.toml"),
            "# Injection is on, deliberately.\n\
             injection_turn_budget = 400\n\
             denied_paths = [\"ops/**\"]\n\n\
             [interception]\n\
             mode = \"enforce\"\n\
             min_slice_lines = 24\n\n\
             [interception.agents]\n\
             claude-code = \"enforce\"\n\n\
             [search]\n\
             mode = \"enforce\"\n",
        )
        .unwrap();

        let written = write_levers_off(scratch.path()).unwrap();
        assert!(written.ends_with("serve.toml"));

        let policy = ServePolicy::load(&rift_dir);
        assert!(!policy.fail_closed, "off is not the same as refused");
        assert_eq!(policy.interception.mode, Mode::Off);
        assert_eq!(policy.search.mode, Mode::Off);
        // The table the document never carried is written, not defaulted: the
        // shipped default for each of these is `enforce`.
        assert_eq!(policy.doc.mode, Mode::Off);
        assert_eq!(
            policy.interception.agents.for_family("claude-code"),
            Some(Mode::Off),
            "an override replaces its table, so it is switched off too"
        );
        assert_eq!(
            policy.injection_turn_budget, 400,
            "not this harness's to change"
        );
        assert_eq!(policy.denied_paths, vec!["ops/**".to_owned()]);
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(
            text.contains("# Injection is on, deliberately."),
            "the document is edited, not replaced\n{text}"
        );
        assert!(text.contains("min_slice_lines = 24"), "{text}");
    }

    /// A checkout with no overlay at all still gets one that says off — the
    /// absent file means the shipped defaults, and those are on.
    #[test]
    fn an_absent_overlay_is_written_rather_than_assumed() {
        use rift_recall_policy::{Mode, ServePolicy};

        let scratch = tempfile::tempdir().unwrap();
        write_levers_off(scratch.path()).unwrap();
        let policy = ServePolicy::load(&scratch.path().join(".rift"));
        assert_eq!(policy.interception.mode, Mode::Off);
        assert_eq!(policy.search.mode, Mode::Off);
        assert_eq!(policy.doc.mode, Mode::Off);
    }

    /// The enforce arm writes the named lever keys, and only those, at
    /// `enforce` under `[interception]`, through the same writer the off arm
    /// uses: the rest of the document — the other levers' defaults included —
    /// survives untouched.
    #[test]
    fn the_enforce_arm_writes_only_the_named_levers() {
        use rift_recall_policy::{Mode, ServePolicy};

        let scratch = tempfile::tempdir().unwrap();
        let rift_dir = scratch.path().join(".rift");
        std::fs::create_dir_all(rift_dir.join("policies")).unwrap();
        std::fs::write(
            rift_dir.join("policies").join("serve.toml"),
            "# Injection is on, deliberately.\n\
             injection_turn_budget = 400\n\n\
             [interception]\n\
             mode = \"enforce\"\n\
             min_slice_lines = 24\n\
             narrow = \"observe\"\n",
        )
        .unwrap();
        let written =
            write_levers_enforced(scratch.path(), &[Lever::Narrow, Lever::FirstContact]).unwrap();
        assert!(written.ends_with("serve.toml"));
        let policy = ServePolicy::load(&rift_dir);
        assert!(
            !policy.fail_closed,
            "a lever at enforce is a readable policy"
        );
        assert_eq!(policy.interception.narrow, Mode::Enforce);
        assert_eq!(policy.interception.first_contact, Mode::Enforce);
        assert_eq!(
            policy.interception.decline_family_held,
            Mode::Enforce,
            "a lever the line did not name is left as it was"
        );
        assert_eq!(policy.interception.inherit_by_name, Mode::Enforce);
        assert_eq!(policy.interception.mode, Mode::Enforce);
        assert_eq!(policy.injection_turn_budget, 400);
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("# Injection is on, deliberately."), "{text}");
        assert!(text.contains("min_slice_lines = 24"), "{text}");

        let bare = tempfile::tempdir().unwrap();
        write_levers_enforced(bare.path(), &[Lever::InheritByName]).unwrap();
        let policy = ServePolicy::load(&bare.path().join(".rift"));
        assert_eq!(policy.interception.inherit_by_name, Mode::Enforce);
        assert_eq!(policy.interception.narrow, Mode::Observe, "the default");
        assert!(levers_enforced(
            "[interception
",
            &[Lever::Narrow]
        )
        .is_err());
    }

    /// An overlay whose per-family table is inline is the same policy written
    /// differently, and is switched off the same way.
    #[test]
    fn an_inline_override_is_switched_off_too() {
        let out =
            levers_off("[interception]\nmode = \"enforce\"\nagents = { codex = \"enforce\" }\n")
                .unwrap();
        assert!(out.contains("codex = \"off\""), "{out}");
        assert!(levers_off("[interception\n").is_err(), "not TOML");
        assert!(
            levers_off("interception = 3\n").is_err(),
            "a lever table that is not a table"
        );
    }

    /// The table carries both arms side by side, the removal somebody
    /// measured, what the levers booked over the same sessions, and the ratio
    /// between them — per session, per client, and over the run.
    #[test]
    fn the_compare_table_carries_both_arms_and_the_calibration() {
        let run = CompareRun {
            client: Client::ClaudeCode,
            off: arm_run(
                Client::ClaudeCode,
                Arm::Off,
                vec![
                    session(1, 12_000, 0, None, Some(41_000), Status::Completed),
                    session(2, 5_000, 0, None, Some(30_000), Status::Completed),
                ],
            ),
            on: arm_run(
                Client::ClaudeCode,
                Arm::On,
                vec![
                    session(1, 9_000, 2_800, Some(400), Some(38_000), Status::Completed),
                    session(2, 4_000, 900, Some(100), Some(29_000), Status::Completed),
                ],
            ),
        };
        let rows = rows_for(&run);
        assert_eq!(rows.len(), 3, "two sessions and the client's own row");
        assert_eq!(rows[0].measured(), 3_000);
        assert!(rows[0]
            .calibration()
            .is_some_and(|c| (c - 93.333).abs() < 0.01));
        let total = rows.last().unwrap();
        assert_eq!(total.session, SUMMED);
        assert_eq!((total.read_off, total.read_on), (17_000, 13_000));
        assert_eq!(total.measured(), 4_000);
        assert_eq!(total.booked_avoided, 3_700);
        assert_eq!(total.booked_prevented, Some(500));
        assert_eq!(
            (total.reported_off, total.reported_on),
            (Some(71_000), Some(67_000))
        );
        assert!(total.calibration().is_some_and(|c| (c - 92.5).abs() < 1e-9));

        let table = render_compare_table(std::slice::from_ref(&run));
        assert!(
            table.contains("read_off") && table.contains("read_on"),
            "{table}"
        );
        assert!(
            table.contains("92.50%"),
            "the client's calibration\n{table}"
        );
        assert!(table.contains("overall"), "{table}");
        assert!(
            table.contains("41000") && table.contains("38000"),
            "{table}"
        );
        assert!(
            table.contains("never clamped"),
            "the caveat travels with it\n{table}"
        );
        assert!(!table.contains(" !"), "nothing was incomplete\n{table}");
    }

    /// The arm with the levers on read MORE. That is an ordinary outcome for
    /// two non-deterministic runs and the most important thing this table can
    /// say, so it is printed as the negative it is and no ratio is invented
    /// for it.
    #[test]
    fn a_negative_removal_is_reported_not_clamped() {
        let run = CompareRun {
            client: Client::Codex,
            off: arm_run(
                Client::Codex,
                Arm::Off,
                vec![session(1, 5_000, 0, None, None, Status::Completed)],
            ),
            on: arm_run(
                Client::Codex,
                Arm::On,
                vec![session(1, 6_500, 700, Some(200), None, Status::TimedOut)],
            ),
        };
        let rows = rows_for(&run);
        assert_eq!(rows[0].measured(), -1_500);
        assert_eq!(
            rows[0].calibration(),
            None,
            "no ratio over a removal that is not one"
        );
        assert_eq!(fmt_calibration(rows[0].calibration()), "-");
        assert!(!rows[0].both_completed, "one arm timed out");
        let table = render_compare_table(std::slice::from_ref(&run));
        assert!(table.contains("-1500"), "the negative is printed\n{table}");
        assert!(
            table.contains("! one arm of this row did not complete"),
            "and the row that is not a measurement says so\n{table}"
        );
        let json = compare_json(std::slice::from_ref(&run), "20260909-120000");
        assert_eq!(json["rows"][0]["measured"], -1_500);
        assert!(json["rows"][0]["calibration_pct"].is_null());
        assert_eq!(json["rows"][0]["both_arms_completed"], false);
    }

    /// A prevented figure the reporting surface never carried is a dash in
    /// every row it reaches, including the sums — a total that read silence as
    /// zero would understate itself.
    #[test]
    fn an_absent_prevented_figure_renders_as_a_dash() {
        let run = CompareRun {
            client: Client::Antigravity,
            off: arm_run(
                Client::Antigravity,
                Arm::Off,
                vec![
                    session(1, 8_000, 0, None, None, Status::Completed),
                    session(2, 2_000, 0, None, None, Status::Completed),
                ],
            ),
            on: arm_run(
                Client::Antigravity,
                Arm::On,
                vec![
                    session(1, 6_000, 500, Some(120), None, Status::Completed),
                    session(2, 1_000, 300, None, None, Status::Completed),
                ],
            ),
        };
        let rows = rows_for(&run);
        assert_eq!(rows[0].booked_prevented, Some(120));
        assert_eq!(rows[1].booked_prevented, None);
        assert_eq!(
            rows[2].booked_prevented, None,
            "one silent session leaves the total silent"
        );
        assert_eq!(fmt_opt(rows[1].booked_prevented), "-");
        let table = render_compare_table(std::slice::from_ref(&run));
        assert!(table.contains('-'), "{table}");
        assert!(
            table.contains("          -"),
            "an absent figure is a dash in its own column\n{table}"
        );
        let json = compare_json(std::slice::from_ref(&run), "s");
        assert!(json["rows"][1]["booked_prevented"].is_null());
        assert!(json["rows"][1]["client_reported_off"].is_null());
    }

    /// The document carries the rows the table drew, both arms' families, and
    /// the sentence the numbers cannot carry on their own.
    #[test]
    fn compare_json_carries_the_caveat() {
        let run = CompareRun {
            client: Client::ClaudeCode,
            off: arm_run(
                Client::ClaudeCode,
                Arm::Off,
                vec![session(1, 12_000, 0, None, Some(41_000), Status::Completed)],
            ),
            on: arm_run(
                Client::ClaudeCode,
                Arm::On,
                vec![session(
                    1,
                    9_000,
                    2_800,
                    Some(400),
                    Some(38_000),
                    Status::Completed,
                )],
            ),
        };
        let json = compare_json(std::slice::from_ref(&run), "20260909-120000");
        assert_eq!(json["kind"], "dogfood-calibration");
        assert_eq!(json["stamp"], "20260909-120000");
        let caveat = json["caveat"].as_str().unwrap();
        assert!(caveat.contains("non-deterministic"), "{caveat}");
        assert!(caveat.contains("never clamped"), "{caveat}");
        // The table breaks that sentence on spaces; the document keeps it as
        // one string.
        assert!(!caveat.contains('\n'), "{caveat}");
        assert!(
            wrapped(caveat, 76).iter().all(|l| l.chars().count() <= 76),
            "the printed caveat fits a terminal"
        );
        assert_eq!(json["arms"][0], "off");
        assert_eq!(json["arms"][1], "on");
        let client = &json["clients"][0];
        assert_eq!(client["client"], "claude-code");
        assert_ne!(
            client["off"]["recall_db"], client["on"]["recall_db"],
            "the arms never share a family"
        );
        assert_eq!(json["rows"][0]["read_off"], 12_000);
        assert_eq!(json["rows"][0]["read_on"], 9_000);
        assert_eq!(json["rows"][0]["booked_avoided"], 2_800);
        assert_eq!(json["rows"][0]["booked_prevented"], 400);
        assert_eq!(json["rows"][0]["booked_returned"], 25);
        assert!(json["rows"][0]["calibration_pct"]
            .as_f64()
            .is_some_and(|c| (c - 93.333).abs() < 0.01));
        // A client row, then the run's own row.
        assert_eq!(json["rows"][1]["session"], SUMMED);
        assert_eq!(json["rows"][2]["client"], "overall");
    }

    /// Two turns are one session's cost on the client that states a total per
    /// turn.
    #[test]
    fn a_client_that_states_a_total_per_turn_is_summed() {
        let log = "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10}}\n\
                   {\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":32}}\n";
        assert_eq!(client_reported_tokens(Client::Codex, log), Some(42));
    }
}
