//! Where each stage time of a read comes from, read-only, and the join each
//! one rested on.
//!
//! * **Receipt** is the caller's: the live window-journal line the read gate
//!   wrote, with the native invocation id when the hook event carried one.
//! * **Durable capture** is the capture projection's `checkpoint.created_at`
//!   for the checkpoint whose `n_window_row` holds the read.
//! * **Query readiness** is the earliest `ingest_watermark.ingested_at` whose
//!   walk position covers that checkpoint's ordinal.
//! * **Final delivery** is the emitted call fact with the read's invocation id
//!   when one was written whole, and otherwise the window's first later
//!   `deny`, `hold` or `prevented` row on the same path and content hash.
//!
//! A serve row names no path and no hash, so it is never a source here. No
//! stage is ever matched by time alone: no match leaves it absent, and two
//! reads the available key cannot tell apart leave it ambiguous rather than
//! credited to either.

use rift_core::Result;
use rift_store::{CanonicalStore, Value};
use serde::Serialize;

use super::ReadStageTimes;

/// The `n_window_row` kinds a read's own line is written as.
const READ_KINDS: &str = "('read', 'slice')";
/// The `n_window_row` kinds that answer a read in place of its lines.
const ANSWER_KINDS: &str = "('deny', 'hold', 'prevented')";

/// One read the gate saw, as its live window-journal line states it.
#[derive(Clone, Copy, Debug)]
pub struct ReadReceipt<'a> {
    /// The context window's journal key.
    pub window_key: &'a str,
    /// The file, in the spelling coverage is keyed by.
    pub path: &'a str,
    /// The revision the bytes hashed to when the read was seen.
    pub content_sha256: &'a str,
    /// When the line was written, epoch milliseconds.
    pub received_at: i64,
    /// The client's own id for the call, when its hook event carried one.
    pub invocation_id: Option<&'a str>,
}

/// One `emitted` fact from a window's call sidecar.
#[derive(Clone, Copy, Debug)]
pub struct CallEmission<'a> {
    /// The client's own id for the call.
    pub invocation_id: &'a str,
    /// Whether the whole response was written to stdout; one that was not
    /// never reached the agent and is no delivery.
    pub wrote: bool,
    /// When the fact was written, epoch milliseconds.
    pub at_ms: i64,
}

/// The key a stage was matched on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageJoin {
    /// The client's own call id.
    InvocationId,
    /// The window, the path and the content hash.
    PathHash,
}

/// One stage of one read as the stores state it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StageFact {
    /// Nothing in the stores matched the read.
    Absent,
    /// One match, stamped `at_ms`.
    At {
        /// When the stage was reached, epoch milliseconds.
        at_ms: i64,
        /// The key the match rested on.
        join: StageJoin,
    },
    /// More than one read shares the key, so the match belongs to neither.
    Ambiguous {
        /// The key that could not tell the reads apart.
        join: StageJoin,
    },
}

impl StageFact {
    fn at(self) -> Option<i64> {
        match self {
            Self::At { at_ms, .. } => Some(at_ms),
            Self::Absent | Self::Ambiguous { .. } => None,
        }
    }

    fn at_by_path_hash(at_ms: Option<i64>) -> Self {
        at_ms.map_or(Self::Absent, |at_ms| Self::At {
            at_ms,
            join: StageJoin::PathHash,
        })
    }
}

/// The four stages of one read, each with its join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReadStageFacts {
    /// When the read gate wrote the read's line.
    pub received_at: i64,
    /// Durable capture.
    pub captured: StageFact,
    /// Query readiness.
    pub queryable: StageFact,
    /// Final delivery.
    pub delivered: StageFact,
}

impl ReadStageFacts {
    /// The stage times the report arithmetic takes; an ambiguous stage counts
    /// as not reached, since it cannot be credited to this read.
    #[must_use]
    pub fn times(&self) -> ReadStageTimes {
        ReadStageTimes {
            received_at: self.received_at,
            captured_at: self.captured.at(),
            queryable_at: self.queryable.at(),
            delivered_at: self.delivered.at(),
        }
    }
}

/// The two stores the stage times are read from.
#[derive(Clone, Copy)]
pub struct StageStores<'a> {
    /// The family's recall store.
    pub recall: &'a dyn CanonicalStore,
    /// The family's capture projection.
    pub capture: &'a dyn CanonicalStore,
}

/// The stage facts of every receipt, in receipt order.
///
/// # Errors
/// A store query fails.
pub async fn read_stage_facts(
    stores: StageStores<'_>,
    receipts: &[ReadReceipt<'_>],
    calls: &[CallEmission<'_>],
) -> Result<Vec<ReadStageFacts>> {
    let mut out = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        let path_hash_twins = receipts.iter().filter(|r| same_key(r, receipt)).count();
        out.push(stage_facts(stores, receipts, receipt, path_hash_twins, calls).await?);
    }
    Ok(out)
}

async fn stage_facts(
    stores: StageStores<'_>,
    receipts: &[ReadReceipt<'_>],
    receipt: &ReadReceipt<'_>,
    path_hash_twins: usize,
    calls: &[CallEmission<'_>],
) -> Result<ReadStageFacts> {
    let checkpoints = read_row_checkpoints(stores.recall, receipt).await?;
    let ambiguous = path_hash_twins.max(checkpoints.len()) > 1;
    let (captured, queryable) = match checkpoints.as_slice() {
        _ if ambiguous => (ambiguous_by_path_hash(), ambiguous_by_path_hash()),
        [checkpoint] => (
            StageFact::at_by_path_hash(sealed_at(stores.capture, checkpoint).await?),
            StageFact::at_by_path_hash(ingested_at(stores.recall, checkpoint).await?),
        ),
        _ => (StageFact::Absent, StageFact::Absent),
    };
    let delivered = match delivery_by_invocation(receipts, receipt, calls) {
        StageFact::Absent if ambiguous => ambiguous_by_path_hash(),
        StageFact::Absent => StageFact::at_by_path_hash(answered_at(stores.recall, receipt).await?),
        fact => fact,
    };
    Ok(ReadStageFacts {
        received_at: receipt.received_at,
        captured,
        queryable,
        delivered,
    })
}

const fn ambiguous_by_path_hash() -> StageFact {
    StageFact::Ambiguous {
        join: StageJoin::PathHash,
    }
}

fn same_key(a: &ReadReceipt<'_>, b: &ReadReceipt<'_>) -> bool {
    a.window_key == b.window_key && a.path == b.path && a.content_sha256 == b.content_sha256
}

/// Delivery by the read's own call id: the earliest emission written whole.
fn delivery_by_invocation(
    receipts: &[ReadReceipt<'_>],
    receipt: &ReadReceipt<'_>,
    calls: &[CallEmission<'_>],
) -> StageFact {
    let Some(id) = receipt.invocation_id else {
        return StageFact::Absent;
    };
    let Some(at_ms) = calls
        .iter()
        .filter(|call| call.wrote && call.invocation_id == id)
        .map(|call| call.at_ms)
        .min()
    else {
        return StageFact::Absent;
    };
    let join = StageJoin::InvocationId;
    if receipts
        .iter()
        .filter(|r| r.invocation_id == Some(id))
        .count()
        > 1
    {
        return StageFact::Ambiguous { join };
    }
    StageFact::At { at_ms, join }
}

fn key_params(receipt: &ReadReceipt<'_>) -> Vec<Value> {
    vec![
        Value::text(receipt.window_key),
        Value::text(receipt.path),
        Value::text(receipt.content_sha256),
    ]
}

/// The checkpoint of every stored read row on the receipt's key.
async fn read_row_checkpoints(
    recall: &dyn CanonicalStore,
    receipt: &ReadReceipt<'_>,
) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT checkpoint_id FROM n_window_row WHERE window_key = ?1 AND path = ?2 \
         AND content_sha256 = ?3 AND kind IN {READ_KINDS} ORDER BY at"
    );
    let rows = recall.query(&sql, &key_params(receipt)).await?;
    rows.iter().map(|row| row.get_text(0)).collect()
}

/// The first column of the first row, or nothing when no row or a null came
/// back.
async fn first_int(store: &dyn CanonicalStore, sql: &str, params: &[Value]) -> Result<Option<i64>> {
    let rows = store.query(sql, params).await?;
    match rows.first() {
        Some(row) => row.get_opt_i64(0),
        None => Ok(None),
    }
}

/// When the capture projection sealed `checkpoint`.
async fn sealed_at(capture: &dyn CanonicalStore, checkpoint: &str) -> Result<Option<i64>> {
    let sql = "SELECT created_at FROM checkpoint WHERE checkpoint_id = ?1";
    first_int(capture, sql, &[Value::text(checkpoint)]).await
}

/// The earliest ingest whose walk position reached `checkpoint`'s ordinal.
async fn ingested_at(recall: &dyn CanonicalStore, checkpoint: &str) -> Result<Option<i64>> {
    let sql = "SELECT MIN(w.ingested_at) FROM ingest_watermark w \
               JOIN ingest_watermark_position p ON p.watermark_id = w.watermark_id \
               JOIN n_checkpoint c ON c.node_id = ?1 WHERE p.chain_seq >= c.ordinal";
    first_int(recall, sql, &[Value::text(checkpoint)]).await
}

/// The window's first answer row on the receipt's key, at or after receipt.
async fn answered_at(
    recall: &dyn CanonicalStore,
    receipt: &ReadReceipt<'_>,
) -> Result<Option<i64>> {
    let sql = format!(
        "SELECT MIN(at) FROM n_window_row WHERE window_key = ?1 AND path = ?2 \
         AND content_sha256 = ?3 AND kind IN {ANSWER_KINDS} AND at >= ?4"
    );
    let mut params = key_params(receipt);
    params.push(Value::Int(receipt.received_at));
    first_int(recall, &sql, &params).await
}

#[cfg(test)]
mod tests;
