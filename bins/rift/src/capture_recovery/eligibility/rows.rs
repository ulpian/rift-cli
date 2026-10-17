//! The guarded point queries and their typed cells.
//!
//! `CanonicalStore::query` returns dynamically typed cells, so `LIMIT` alone
//! bounds nothing: every selected value is wrapped in a type-and-size `CASE`
//! before SQLite materialises it, and a `NULL` out of a guard is the INVALID
//! sentinel — the row is kept and classified unavailable, never read as an
//! absent fact. No guard sits in a `WHERE`, so a malformed row cannot hide.
//! Bounds reuse existing contracts: 64 hex for every id and digest, the
//! longest closed token for agents, kinds, trusts, outcomes and edges, the
//! session-id ceiling for a native id, the plan's slug ceiling for a repo id.
use rift_store::Row;

use super::Reason;

/// `claude-code` and `antigravity`, the longest closed owner tokens.
pub(super) const MAX_AGENT_BYTES: usize = 11;
/// `session_compaction`, the longest `EventKind` token.
const MAX_KIND_BYTES: usize = 18;
/// `tool_output_untrusted`, the longest `Trust` token.
const MAX_TRUST_BYTES: usize = 21;
/// `skipped-unknown-verb`, the longest receipt outcome.
const MAX_OUTCOME_BYTES: usize = 20;
/// `start` | `end`.
const MAX_EDGE_BYTES: usize = 5;
/// The declared-verb ceiling the receipt preflight enforces.
pub(super) const MAX_VERB_BYTES: usize = 128;
/// `MAX_SESSION_ID_BYTES`.
const MAX_NATIVE_BYTES: usize = rift_shadow::hooks::MAX_SESSION_ID_BYTES;
/// `RepoId::new`'s slug ceiling.
const MAX_REPO_BYTES: usize = 128;

macro_rules! text_guard {
    ($column:literal, $max:literal) => {
        concat!(
            "CASE WHEN typeof(",
            $column,
            ")='text' AND length(CAST(",
            $column,
            " AS BLOB))<=",
            $max,
            " THEN ",
            $column,
            " ELSE NULL END"
        )
    };
}
macro_rules! u32_guard {
    ($column:literal) => {
        concat!(
            "CASE WHEN typeof(",
            $column,
            ")='integer' AND ",
            $column,
            " BETWEEN 0 AND 4294967295 THEN ",
            $column,
            " ELSE NULL END"
        )
    };
}
macro_rules! schema_guard {
    ($column:literal) => {
        concat!(
            "CASE WHEN typeof(",
            $column,
            ")='integer' AND ",
            $column,
            "=1 THEN ",
            $column,
            " ELSE NULL END"
        )
    };
}

pub(super) const IDENTITY_BY_PK: &str = concat!(
    "SELECT ",
    text_guard!("session_id", 64),
    ",",
    text_guard!("agent", 11),
    ",",
    text_guard!("native_session_id", 200),
    ",",
    schema_guard!("schema_version"),
    " FROM capture_session_identity WHERE session_id=?1 LIMIT 1"
);
pub(super) const SESSION_BY_PK: &str = concat!(
    "SELECT ",
    text_guard!("session_id", 64),
    ",",
    text_guard!("agent", 11),
    ",",
    text_guard!("repo_id", 128),
    ",",
    schema_guard!("schema_version"),
    " FROM session WHERE session_id=?1 LIMIT 1"
);
pub(super) const TURN_BY_PK: &str = concat!(
    "SELECT ",
    text_guard!("turn_id", 64),
    ",",
    text_guard!("session_id", 64),
    ",",
    u32_guard!("ordinal"),
    ",",
    schema_guard!("schema_version"),
    " FROM turn WHERE turn_id=?1 LIMIT 1"
);
macro_rules! witness {
    ($kinds:literal) => {
        concat!(
            "SELECT ",
            text_guard!("idempotency_key", 64),
            ",",
            u32_guard!("event_ordinal"),
            ",",
            text_guard!("kind", 18),
            ",",
            text_guard!("trust", 21),
            ",",
            text_guard!("payload_sha256", 64),
            ",",
            schema_guard!("schema_version"),
            " FROM captured_event WHERE session_id=?1 AND turn_ordinal=?2 AND ",
            $kinds,
            " LIMIT 2"
        )
    };
}
pub(super) const START_WITNESS: &str = witness!("kind IN ('turn_start','prompt_submit')");
pub(super) const END_WITNESS: &str = witness!("kind='turn_end'");
/// A receipt's `session_id` is nullable by design (a session-less receipt), so
/// a real NULL reads as the empty string, which no safe native id is; the
/// guard's NULL stays the invalid sentinel.
pub(super) const RECEIPT_BY_PK: &str = concat!(
    "SELECT ", text_guard!("hook_event_id", 64), ",", text_guard!("agent", 11), ",",
    text_guard!("verb", 128), ",",
    "CASE WHEN session_id IS NULL THEN '' WHEN typeof(session_id)='text' AND length(CAST(session_id AS BLOB))<=200 THEN session_id ELSE NULL END,",
    text_guard!("outcome", 20), ",", schema_guard!("schema_version"),
    " FROM agent_hook_event WHERE hook_event_id=?1 LIMIT 1"
);
pub(super) const TARGET_BY_PK: &str = concat!(
    "SELECT ",
    text_guard!("snapshot_id", 64),
    ",",
    text_guard!("session_id", 64),
    ",",
    u32_guard!("turn_ordinal"),
    ",",
    text_guard!("boundary", 5),
    ",",
    schema_guard!("schema_version"),
    " FROM turn_boundary_snapshot WHERE snapshot_id=?1 LIMIT 1"
);
pub(super) const LATER_EXISTS: &str =
    "SELECT 1 FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal>?2 LIMIT 1";
pub(super) const SAME_TURN: &str = concat!(
    "SELECT ",
    text_guard!("snapshot_id", 64),
    ",",
    text_guard!("boundary", 5),
    ",",
    schema_guard!("schema_version"),
    " FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal=?2 LIMIT 3"
);
pub(super) const OBSERVATION_EXISTS: &str =
    "SELECT 1 FROM turn_boundary_observation WHERE snapshot_id=?1 LIMIT 1";
pub(super) const FILE_EXISTS: &str =
    "SELECT 1 FROM turn_boundary_file WHERE snapshot_id=?1 LIMIT 1";
pub(super) const FILE_KIND_EXISTS: &str =
    "SELECT 1 FROM turn_boundary_file_kind WHERE snapshot_id=?1 LIMIT 1";

/// The guard shapes over a bound parameter, for proving them against the
/// engine's own typing (the canonical tables are `STRICT`, so a wrongly typed
/// cell cannot be seeded into one).
#[cfg(test)]
pub(super) const U32_PARAM_GUARD: &str = u32_guard!("?1");
#[cfg(test)]
pub(super) const TEXT64_PARAM_GUARD: &str = text_guard!("?1", 64);
#[cfg(test)]
pub(super) const SCHEMA_PARAM_GUARD: &str = schema_guard!("?1");

pub(super) struct IdentityFact {
    pub(super) session_id: String,
    pub(super) agent: String,
    pub(super) native: String,
}

pub(super) struct SessionFact {
    pub(super) session_id: String,
    pub(super) agent: String,
    pub(super) repo_id: String,
}

pub(super) struct TurnFact {
    pub(super) turn_id: String,
    pub(super) session_id: String,
    pub(super) ordinal: u32,
}

pub(super) struct EventFact {
    pub(super) key: String,
    pub(super) event_ordinal: u32,
    pub(super) kind: String,
    pub(super) trust: String,
    pub(super) payload_sha256: String,
}

pub(super) struct ReceiptFact {
    pub(super) id: String,
    pub(super) agent: String,
    pub(super) verb: String,
    pub(super) native: String,
    pub(super) outcome: String,
}

pub(super) struct SnapshotFact {
    pub(super) snapshot_id: String,
    pub(super) session_id: String,
    pub(super) ordinal: u32,
    pub(super) boundary: String,
}

pub(super) struct SameTurnFact {
    pub(super) snapshot_id: String,
    pub(super) boundary: String,
}

/// A guarded text cell: present, text and within `max` bytes.
pub(super) fn text(row: &Row, index: usize, max: usize) -> Result<String, Reason> {
    match row.get_opt_text(index) {
        Ok(Some(value)) if value.len() <= max => Ok(value),
        _ => Err(Reason::CellInvalid),
    }
}

/// A guarded `u32` cell.
pub(super) fn ordinal(row: &Row, index: usize) -> Result<u32, Reason> {
    match row.get_opt_i64(index) {
        Ok(Some(value)) => u32::try_from(value).map_err(|_| Reason::CellInvalid),
        _ => Err(Reason::CellInvalid),
    }
}

/// The schema cell: the guard already requires the integer `1`.
fn schema_one(row: &Row, index: usize) -> Result<(), Reason> {
    match row.get_opt_i64(index) {
        Ok(Some(1)) => Ok(()),
        _ => Err(Reason::CellInvalid),
    }
}

/// A 64-character lowercase hex id or digest.
pub(super) fn hex64(row: &Row, index: usize) -> Result<String, Reason> {
    let value = text(row, index, 64)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Reason::CellInvalid);
    }
    Ok(value)
}

fn width(row: &Row, columns: usize) -> Result<(), Reason> {
    if row.len() != columns {
        return Err(Reason::CellInvalid);
    }
    Ok(())
}

pub(super) fn identity(row: &Row) -> Result<IdentityFact, Reason> {
    width(row, 4)?;
    schema_one(row, 3)?;
    Ok(IdentityFact {
        session_id: hex64(row, 0)?,
        agent: text(row, 1, MAX_AGENT_BYTES)?,
        native: text(row, 2, MAX_NATIVE_BYTES)?,
    })
}

pub(super) fn session(row: &Row) -> Result<SessionFact, Reason> {
    width(row, 4)?;
    schema_one(row, 3)?;
    Ok(SessionFact {
        session_id: hex64(row, 0)?,
        agent: text(row, 1, MAX_AGENT_BYTES)?,
        repo_id: text(row, 2, MAX_REPO_BYTES)?,
    })
}

pub(super) fn turn(row: &Row) -> Result<TurnFact, Reason> {
    width(row, 4)?;
    schema_one(row, 3)?;
    Ok(TurnFact {
        turn_id: hex64(row, 0)?,
        session_id: hex64(row, 1)?,
        ordinal: ordinal(row, 2)?,
    })
}

pub(super) fn event(row: &Row) -> Result<EventFact, Reason> {
    width(row, 6)?;
    schema_one(row, 5)?;
    Ok(EventFact {
        key: hex64(row, 0)?,
        event_ordinal: ordinal(row, 1)?,
        kind: text(row, 2, MAX_KIND_BYTES)?,
        trust: text(row, 3, MAX_TRUST_BYTES)?,
        payload_sha256: hex64(row, 4)?,
    })
}

pub(super) fn receipt(row: &Row) -> Result<ReceiptFact, Reason> {
    width(row, 6)?;
    schema_one(row, 5)?;
    Ok(ReceiptFact {
        id: hex64(row, 0)?,
        agent: text(row, 1, MAX_AGENT_BYTES)?,
        verb: text(row, 2, MAX_VERB_BYTES)?,
        native: text(row, 3, MAX_NATIVE_BYTES)?,
        outcome: text(row, 4, MAX_OUTCOME_BYTES)?,
    })
}

pub(super) fn snapshot(row: &Row) -> Result<SnapshotFact, Reason> {
    width(row, 5)?;
    schema_one(row, 4)?;
    Ok(SnapshotFact {
        snapshot_id: hex64(row, 0)?,
        session_id: hex64(row, 1)?,
        ordinal: ordinal(row, 2)?,
        boundary: text(row, 3, MAX_EDGE_BYTES)?,
    })
}

pub(super) fn same_turn(row: &Row) -> Result<SameTurnFact, Reason> {
    width(row, 3)?;
    schema_one(row, 2)?;
    Ok(SameTurnFact {
        snapshot_id: hex64(row, 0)?,
        boundary: text(row, 1, MAX_EDGE_BYTES)?,
    })
}

/// An existence probe's answer: no row, or exactly the literal `1`.
pub(super) fn exists(rows: &[Row]) -> Result<bool, Reason> {
    match rows {
        [] => Ok(false),
        [row] if row.len() == 1 && row.get_i64(0).is_ok_and(|one| one == 1) => Ok(true),
        _ => Err(Reason::CellInvalid),
    }
}

/// What a result set costs the materialised-cell budget: text and blob bytes,
/// eight per integer, nothing for the sentinel.
pub(super) fn cell_bytes(rows: &[Row]) -> u64 {
    rows.iter()
        .map(|row| {
            (0..row.len())
                .map(
                    |index| match (row.get_opt_text(index), row.get_blob(index)) {
                        (Ok(Some(text)), _) => text.len() as u64,
                        (_, Ok(blob)) => blob.len() as u64,
                        (Ok(None), _) => 0,
                        _ => 8,
                    },
                )
                .sum::<u64>()
        })
        .sum()
}
