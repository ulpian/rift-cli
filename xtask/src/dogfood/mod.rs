//! `cargo run -p xtask -- dogfood` — the scripted-session savings reading.
//!
//! A developer with Claude Code, Codex and Antigravity installed can play a
//! fixed set of short sessions through each client and read one table:
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
//! the run is what it was, byte for byte. `--bypass-shell`, beside
//! `--compare`, adds a third arm that reads only through the client's shell —
//! the shape most of the field's traffic has — and prints it beside the on
//! arm; `shell` holds that arm, and the calibration is unchanged by it.
//! `--enforce <lever>[,…]` writes the named read levers at `enforce` in the
//! checkouts the sessions play in — the off arm excepted — so a lever still
//! at `observe` can be played as it would act; the report names what was
//! written. Without it nothing is written.

mod client;
mod harness;
mod measure;
mod options;
mod process;
mod report;
mod scratch;
mod sessions;
mod shell;

pub use client::{Client, ShellArm, ALL_CLIENTS};
pub use harness::run;
pub use measure::{
    classify_verb, parse_kinds, parse_savings, retries_after_deny, tally_kinds, unavailability,
    DecisionKind, LeverKind, RetryCounts, SavingsDeltas, SavingsSample,
};
pub use options::{parse_args, Lever, Options};
pub use report::{
    fmt_share, render_table, report_json, share_of, utc_stamp, ClientRun, SessionOutcome, Status,
};
pub use scratch::{ledger_lines, refuse_shared_family, Prepared};
pub use sessions::{load_sessions, parse_session, Session, SESSIONS_DIR};

#[cfg(test)]
pub(super) mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A `rift savings --json` document captured from a live family, with the
    /// home directory generalised. The parser is held to real output rather
    /// than to a shape somebody remembered.
    pub(super) const SAVINGS_SAMPLE: &str =
        include_str!("../../dogfood/testdata/savings-sample.json");
    /// The `levers` half of a live `rift memory reread-report --json`.
    pub(super) const REREAD_SAMPLE: &str =
        include_str!("../../dogfood/testdata/reread-sample.json");

    /// One client's run, shaped so every surface that draws it has something
    /// to draw: a completed session with figures and a refused one without.
    pub(super) fn sample_run(
        client: Client,
        savings: SavingsSample,
        kinds: Vec<LeverKind>,
    ) -> ClientRun {
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
}
