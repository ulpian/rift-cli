//! The four stages one read passes on its way to a served answer, kept as
//! separate timestamped facts so a report can say which stage a read reached
//! and how long each step took.
//!
//! * **Receipt**: the read gate saw the read and wrote its window-journal line.
//! * **Durable capture**: a checkpoint froze that line into the native archive.
//! * **Query readiness**: recall ingest walked that checkpoint and wrote the
//!   bound row a query can find.
//! * **Final delivery**: an answer built on the read reached the agent.
//!
//! Capture and readiness form one chain, the archive chain: readiness without
//! a capture is a gap, never a skipped step. Delivery hangs off receipt alone,
//! because the hook channel can answer from the observed live head before the
//! turn's checkpoint is sealed; a delivery ahead of capture is honest, not out
//! of order. Archival lag, query lag and delivery latency stay three numbers so
//! a slow archive is never read as a slow answer, or the other way round.
//!
//! Read-only arithmetic over facts other code already wrote: this module opens
//! no store and adds no index.

mod sources;

use serde::Serialize;

pub use sources::{
    read_stage_facts, CallEmission, ReadReceipt, ReadStageFacts, StageFact, StageJoin, StageStores,
};

/// How far along the archive chain a read has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStage {
    /// The read gate saw the read; no checkpoint holds it yet.
    Received,
    /// A checkpoint froze the read; recall ingest has not reached it.
    Captured,
    /// Recall ingest wrote the bound row a query can find.
    Queryable,
}

/// The four stage times for one read, epoch milliseconds. Receipt is required:
/// without it there is no read to report on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStageTimes {
    /// When the read gate wrote the read's window-journal line.
    pub received_at: i64,
    /// When the checkpoint holding that line was sealed.
    pub captured_at: Option<i64>,
    /// When recall ingest covered that checkpoint.
    pub queryable_at: Option<i64>,
    /// When an answer built on the read reached the agent.
    pub delivered_at: Option<i64>,
}

/// Something about a read's stages a reader must not take at face value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StageAnomaly {
    /// A query-ready row with no capture behind it: the archive chain has a
    /// hole, so the read is reported as received only.
    QueryableWithoutCapture,
    /// A stage stamped earlier than the stage it follows, by `by_ms`. The lag
    /// for that step is withheld rather than reported as a negative number.
    ClockSkew {
        /// The stage stamped too early.
        stage: Stage,
        /// How far before its predecessor it was stamped.
        by_ms: i64,
    },
}

/// A stage name for anomalies, covering delivery as well as the archive chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Durable capture.
    Captured,
    /// Query readiness.
    Queryable,
    /// Final delivery.
    Delivered,
}

/// What one read reached and how long each step took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReadStageReport {
    /// The furthest archive stage reached with no hole before it.
    pub archived_to: ArchiveStage,
    /// Whether an answer built on the read reached the agent.
    pub delivered: bool,
    /// Receipt to durable capture.
    pub archival_lag_ms: Option<i64>,
    /// Durable capture to query readiness.
    pub query_lag_ms: Option<i64>,
    /// Receipt to final delivery.
    pub delivery_latency_ms: Option<i64>,
    /// Holes and skewed stamps, in stage order.
    pub anomalies: Vec<StageAnomaly>,
}

/// Report the stages one read reached and the lag of each step.
#[must_use]
pub fn report(times: ReadStageTimes) -> ReadStageReport {
    let mut anomalies = Vec::new();
    let archival_lag_ms = step_lag(
        times.received_at,
        times.captured_at,
        Stage::Captured,
        &mut anomalies,
    );
    let query_lag_ms = match times.captured_at {
        Some(captured_at) => step_lag(
            captured_at,
            times.queryable_at,
            Stage::Queryable,
            &mut anomalies,
        ),
        None => None,
    };
    let delivery_latency_ms = step_lag(
        times.received_at,
        times.delivered_at,
        Stage::Delivered,
        &mut anomalies,
    );
    if times.captured_at.is_none() && times.queryable_at.is_some() {
        anomalies.insert(0, StageAnomaly::QueryableWithoutCapture);
    }
    ReadStageReport {
        archived_to: archived_to(times),
        delivered: times.delivered_at.is_some(),
        archival_lag_ms,
        query_lag_ms,
        delivery_latency_ms,
        anomalies,
    }
}

/// The furthest archive stage reached without a hole before it.
fn archived_to(times: ReadStageTimes) -> ArchiveStage {
    match (times.captured_at, times.queryable_at) {
        (None, _) => ArchiveStage::Received,
        (Some(_), None) => ArchiveStage::Captured,
        (Some(_), Some(_)) => ArchiveStage::Queryable,
    }
}

/// The lag from `from` to `to`, or nothing when `to` is absent; a negative lag
/// is recorded as clock skew and withheld.
fn step_lag(
    from: i64,
    to: Option<i64>,
    stage: Stage,
    anomalies: &mut Vec<StageAnomaly>,
) -> Option<i64> {
    let to = to?;
    let lag = to.saturating_sub(from);
    if lag < 0 {
        anomalies.push(StageAnomaly::ClockSkew {
            stage,
            by_ms: lag.saturating_neg(),
        });
        return None;
    }
    Some(lag)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVED: i64 = 1_000;

    fn times() -> ReadStageTimes {
        ReadStageTimes {
            received_at: RECEIVED,
            ..ReadStageTimes::default()
        }
    }

    #[test]
    fn a_read_only_the_hook_saw_is_received_and_undelivered() {
        let r = report(times());
        assert_eq!(r.archived_to, ArchiveStage::Received);
        assert!(!r.delivered);
        assert_eq!(
            (r.archival_lag_ms, r.query_lag_ms, r.delivery_latency_ms),
            (None, None, None)
        );
        assert!(r.anomalies.is_empty());
    }

    #[test]
    fn a_read_through_every_stage_keeps_archival_query_and_delivery_lags_apart() {
        let r = report(ReadStageTimes {
            captured_at: Some(RECEIVED + 30_000),
            queryable_at: Some(RECEIVED + 30_400),
            delivered_at: Some(RECEIVED + 120),
            ..times()
        });
        assert_eq!(r.archived_to, ArchiveStage::Queryable);
        assert!(r.delivered);
        assert_eq!(r.archival_lag_ms, Some(30_000));
        assert_eq!(r.query_lag_ms, Some(400));
        assert_eq!(r.delivery_latency_ms, Some(120));
        assert!(
            r.anomalies.is_empty(),
            "delivery ahead of capture is honest: {:?}",
            r.anomalies
        );
    }

    #[test]
    fn a_captured_read_not_yet_ingested_stops_at_captured_with_no_query_lag() {
        let r = report(ReadStageTimes {
            captured_at: Some(RECEIVED + 5),
            ..times()
        });
        assert_eq!(r.archived_to, ArchiveStage::Captured);
        assert_eq!((r.archival_lag_ms, r.query_lag_ms), (Some(5), None));
    }

    #[test]
    fn a_queryable_row_without_a_capture_is_named_as_a_hole_not_promoted() {
        let r = report(ReadStageTimes {
            queryable_at: Some(RECEIVED + 9),
            ..times()
        });
        assert_eq!(r.archived_to, ArchiveStage::Received);
        assert_eq!(r.query_lag_ms, None);
        assert_eq!(r.anomalies, vec![StageAnomaly::QueryableWithoutCapture]);
    }

    #[test]
    fn a_stage_stamped_before_its_predecessor_withholds_its_lag_as_clock_skew() {
        let r = report(ReadStageTimes {
            captured_at: Some(RECEIVED + 50),
            queryable_at: Some(RECEIVED + 20),
            delivered_at: Some(RECEIVED - 7),
            ..times()
        });
        assert_eq!(r.archived_to, ArchiveStage::Queryable);
        assert_eq!((r.query_lag_ms, r.delivery_latency_ms), (None, None));
        assert_eq!(
            r.anomalies,
            vec![
                StageAnomaly::ClockSkew {
                    stage: Stage::Queryable,
                    by_ms: 30
                },
                StageAnomaly::ClockSkew {
                    stage: Stage::Delivered,
                    by_ms: 7
                },
            ]
        );
    }

    #[test]
    fn the_report_serializes_each_stage_and_lag_under_its_own_name() {
        let r = report(ReadStageTimes {
            captured_at: Some(RECEIVED + 1),
            queryable_at: Some(RECEIVED - 1),
            ..times()
        });
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["archived_to"], "queryable");
        assert_eq!(json["delivered"], false);
        assert_eq!(json["archival_lag_ms"], 1);
        assert!(json["query_lag_ms"].is_null());
        assert_eq!(json["anomalies"][0]["kind"], "clock_skew");
        assert_eq!(json["anomalies"][0]["stage"], "queryable");
        assert_eq!(json["anomalies"][0]["by_ms"], 2);
    }
}
