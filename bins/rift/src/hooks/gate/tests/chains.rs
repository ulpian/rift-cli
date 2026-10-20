//! A command line of several reads where some are only partly held: declined
//! only when every member is held or narrowable, answered one sentence per
//! member, and run whole the moment one member is neither.

use std::path::Path;

use rift_agent::window_journal::{Interval, Line, Mode};
use rift_shadow::hooks::RawHook;

use super::fixtures::*;
use crate::hooks::gate::gate_limits;

/// One Codex checkout of two sixty-line files, `src.rs` and `other.rs`, and a
/// third, `third.rs`, that no walk here reads before the call it is about.
struct Chain {
    tmp: tempfile::TempDir,
    /// The family enforcing, with every per-lever key left as it ships.
    enforce: rift_recall_policy::Effective,
    /// The same family with the narrowing lever written `enforce`.
    narrowing: rift_recall_policy::Effective,
}

impl Chain {
    fn here(&self) -> &Path {
        self.tmp.path()
    }

    /// The command line `members` spells, run from this checkout.
    fn line(&self, members: &[&str]) -> RawHook {
        let command = format!(
            "cd {} && {}",
            shell_spelling(self.here()),
            members.join(" && ")
        );
        hook("codex", "PreToolUse", "s-chain", shell_of(&command))
    }

    /// The gate with the narrowing lever written `enforce`.
    fn enforced(&self, members: &[&str]) -> Option<String> {
        gate_under(&self.line(members), self.here(), &self.narrowing).deny_reason
    }

    /// The gate under the default policy: the family enforcing, the
    /// narrowing lever unwritten.
    fn shipped(&self, members: &[&str]) -> Option<String> {
        gate_under(&self.line(members), self.here(), &self.enforce).deny_reason
    }

    fn lines(&self) -> Vec<Line> {
        lines_of(self.here(), "s-chain", None)
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.lines().iter().map(Line::kind).collect()
    }

    fn runs_held_of(&self, rel: &str) -> Vec<Interval> {
        lines_journal(self.here(), "s-chain")
            .read_coverage(rel, revision(self.here(), rel))
            .runs()
            .to_vec()
    }

    /// Every accounting row the walk left, its fresh id taken out so two
    /// walks' rows compare.
    fn bookings(&self) -> Vec<crate::agent_queue::ServeAudit> {
        audits(self.here())
            .into_iter()
            .map(|row| crate::agent_queue::ServeAudit {
                call_id: String::new(),
                ..row
            })
            .collect()
    }
}

fn chain() -> Chain {
    let (tmp, _) = checkout(60);
    beside(tmp.path(), "other.rs", 60);
    beside(tmp.path(), "third.rs", 60);
    let enforce = levers_for("codex", rift_recall_policy::Mode::Enforce);
    let mut narrowing = enforce;
    narrowing.narrow = rift_recall_policy::Mode::Enforce;
    Chain {
        tmp,
        enforce,
        narrowing,
    }
}

/// The plain frame the renderer writes for the whole of `rel`.
fn plain_frame(chain: &Chain, rel: &str) -> String {
    rift_recall_serve::render_deny_frame(rel, &revision(chain.here(), rel).to_hex(), 1, 60, Some(4))
}

/// The narrowing frame the renderer writes for `rel` held at `held`.
fn narrow_frame(chain: &Chain, rel: &str, held: (u32, u32), read: (u32, u32)) -> String {
    let hash = revision(chain.here(), rel).to_hex();
    rift_recall_serve::render_narrow_frame(rel, &hash, held, read, Some(4))
}

const CAT_SRC: &str = "cat src.rs";
const OTHER_HEAD: &str = "sed -n '1,40p' other.rs";
const OTHER_ALL: &str = "sed -n '1,60p' other.rs";

/// ★ A chain whose one member is wholly held and whose other is held at its
/// head is declined, with the plain frame then the narrowing frame, each the
/// renderer's; and every member's journal line and booking is the one a lone
/// call of that member leaves.
#[test]
fn a_chain_of_a_held_and_a_narrowable_member_is_declined_one_sentence_each() {
    let joined = chain();
    assert_eq!(joined.enforced(&[CAT_SRC, OTHER_HEAD]), None);
    let reason = joined
        .enforced(&[CAT_SRC, OTHER_ALL])
        .expect("every member is held or narrowable");
    assert_eq!(
        reason.lines().collect::<Vec<_>>(),
        [
            plain_frame(&joined, "src.rs"),
            narrow_frame(&joined, "other.rs", (1, 40), (41, 60)),
        ],
        "one sentence per member, in the order the line names them"
    );

    let lone = chain();
    for member in [CAT_SRC, OTHER_HEAD] {
        assert_eq!(lone.enforced(&[member]), None);
    }
    assert!(lone.enforced(&[CAT_SRC]).is_some());
    assert!(lone.enforced(&[OTHER_ALL]).is_some());
    assert_eq!(joined.kinds(), ["read", "read", "deny", "deny"]);
    assert_eq!(joined.lines(), lone.lines(), "each member's line, as alone");
    assert_eq!(
        joined.bookings(),
        lone.bookings(),
        "each member's booking, as alone: the narrowing books its held run"
    );
    assert_eq!(
        joined
            .bookings()
            .iter()
            .map(|row| row.basis.as_ref().and_then(|basis| basis.lines))
            .collect::<Vec<_>>(),
        [
            Some(crate::agent_queue::AuditLines { lo: 1, hi: 60 }),
            Some(crate::agent_queue::AuditLines { lo: 1, hi: 40 }),
        ]
    );
}

/// ★ The decline is once per key, member by member: the identical chain
/// asked again runs, and the window is credited with every range it read.
#[test]
fn the_identical_chain_after_its_decline_runs_and_records_the_whole_ranges() {
    let chain = chain();
    chain.enforced(&[CAT_SRC, OTHER_HEAD]);
    assert!(chain.enforced(&[CAT_SRC, OTHER_ALL]).is_some());
    assert_eq!(chain.enforced(&[CAT_SRC, OTHER_ALL]), None);
    assert_eq!(
        chain.kinds(),
        ["read", "read", "deny", "deny", "read", "read"]
    );
    assert_eq!(chain.runs_held_of("other.rs"), [Interval { lo: 1, hi: 60 }]);
    assert_eq!(chain.runs_held_of("src.rs"), [Interval { lo: 1, hi: 60 }]);
}

/// ★ One member the window does not hold at all lets the whole line run:
/// every member is recorded as the read it became, and nothing is booked as
/// spared.
#[test]
fn a_chain_with_one_unheld_member_runs_whole_and_books_nothing() {
    let chain = chain();
    chain.enforced(&[CAT_SRC, OTHER_HEAD]);
    let third = "sed -n '1,40p' third.rs";
    assert_eq!(chain.enforced(&[CAT_SRC, OTHER_ALL, third]), None);
    assert_eq!(chain.kinds(), ["read", "read", "read", "read", "read"]);
    assert_eq!(chain.runs_held_of("other.rs"), [Interval { lo: 1, hi: 60 }]);
    assert_eq!(chain.runs_held_of("third.rs"), [Interval { lo: 1, hi: 40 }]);
    assert_eq!(
        chain.bookings(),
        [],
        "no decline travelled, so none is booked"
    );
}

/// ★ A narrowable member whose held run is under the narrowing floor is not
/// declined, so the whole line runs; its observation is booked as one, and
/// nothing as spared.
#[test]
fn a_chain_whose_narrowable_member_is_under_the_floor_runs_whole() {
    let chain = chain();
    let short = format!(
        "sed -n '1,{}p' other.rs",
        rift_agent::window_journal::NARROW_MIN_HELD_LINES - 1
    );
    chain.enforced(&[CAT_SRC, &short]);
    assert_eq!(chain.enforced(&[CAT_SRC, OTHER_ALL]), None);
    assert_eq!(chain.kinds(), ["read", "read", "read", "read"]);
    assert_eq!(
        chain
            .bookings()
            .iter()
            .map(|row| row.decision)
            .collect::<Vec<_>>(),
        [crate::agent_queue::ServeAuditKind::ReadRepeatObserved],
        "an observation sizes the pool and spares nothing"
    );
}

/// ★ Two narrowable members are two narrowing sentences.
#[test]
fn a_chain_of_two_narrowable_members_is_two_narrowing_sentences() {
    let chain = chain();
    chain.enforced(&["sed -n '1,40p' src.rs", OTHER_HEAD]);
    let reason = chain
        .enforced(&["sed -n '1,60p' src.rs", OTHER_ALL])
        .expect("both members are narrowable");
    assert_eq!(
        reason.lines().collect::<Vec<_>>(),
        [
            narrow_frame(&chain, "src.rs", (1, 40), (41, 60)),
            narrow_frame(&chain, "other.rs", (1, 40), (41, 60)),
        ]
    );
}

/// ★ Under the default policy a narrowable member is only observed, so a
/// chain holding one runs whole — its held member included — and the window
/// is credited with what the call delivered. The family enforces and the
/// narrowing key is unwritten: the per-lever key decides, and the family's
/// mode alone never narrows.
#[test]
fn under_the_default_policy_a_chain_with_a_narrowable_member_runs_whole() {
    assert_eq!(rift_agent::window_journal::NARROW_MODE, Mode::Observe);
    let chain = chain();
    assert_eq!(chain.enforce.read_deny, rift_recall_policy::Mode::Enforce);
    assert_eq!(chain.enforce.narrow, rift_recall_policy::Mode::Observe);
    assert_eq!(gate_limits(&chain.enforce).narrow_mode, Mode::Observe);
    assert_eq!(gate_limits(&chain.narrowing).narrow_mode, Mode::Enforce);
    assert_eq!(chain.shipped(&[CAT_SRC, OTHER_HEAD]), None);
    assert_eq!(chain.shipped(&[CAT_SRC, OTHER_ALL]), None);
    assert_eq!(chain.kinds(), ["read", "read", "read", "read"]);
    assert_eq!(chain.runs_held_of("other.rs"), [Interval { lo: 1, hi: 60 }]);
}
