//! End to end, through the shipped binary: what a decision books, and what
//! the lever is worth over a whole scripted session.
//!
//! Two questions and no others: what one decline writes into the two
//! relations a savings figure is summed from, and what a measured session's
//! addressable repeat pool was worth against how much of it the lever
//! removed. Both are answered in figures the product itself booked, priced a
//! second time by the real tokenizer, so no number here is an estimate.
//!
//! What a walk is answered with is `read_gate_decisions_e2e`; what a window
//! may be credited with is `read_gate_deliveries_e2e`. How every walk here is
//! driven is `common/read_gate_fixture.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
use common::rift_cmd;

use rift_store::CanonicalStore as _;

#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;
use read_gate_fixture::{
    declined_with, edit_inserting_line_7b, enforce_at_the_shipped_floor, enforce_for, gate_env,
    hook, json_str, kinds, kinds_of, native_hook, numbered, read_of, read_whole_of, seed_repo,
    windows, SLICE_CEILING,
};

/// The family's one recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn recall_db(repo: &Path, cache: &Path) -> PathBuf {
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the decisions above should have minted the family");
    };
    rift_store::recall_db_under(cache, &family.recall_id)
}

/// Rows out of the family's recall projection, as text, in the order asked.
fn rows(repo: &Path, cache: &Path, sql: &str) -> Vec<Vec<String>> {
    let store = rift_store::open_migrated(recall_db(repo, cache), &rift_store::recall_migrations())
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(sql, &[])
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (0..8)
                    .map_while(|c| {
                        r.get_text(c)
                            .ok()
                            .or_else(|| r.get_i64(c).ok().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .collect()
    })
}

/// ★ One decline books its accounting in **both** relations, with the real
/// tokenizer's count of the slice it saved and of the sentence it cost.
///
/// The two relations answer different questions and only one each: the audit
/// relation is what a savings figure sums, and the call relation is where the
/// channel and the family the decision came from are recorded. A row that
/// booked what a decline avoided and said nothing about what its reason cost
/// would flatter itself, so both figures are asserted, and the returned side
/// is asserted to be non-zero — the frame is a real cost and the honesty gate
/// is the difference, not the avoided column alone.
#[test]
fn a_decline_books_what_it_saved_and_what_it_cost_in_both_relations() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("booked.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let read = read_of(&file, 1, 40);

    hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &read, &env);
    let denied = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &read, &env);
    assert!(declined_with(&denied).is_some(), "the repeat is declined");

    let audit = rows(
        &repo,
        &cache,
        "SELECT verb, handle, reason, tokens_returned, tokens_avoided FROM memd_audit_event",
    );
    let [row] = audit.as_slice() else {
        panic!("one decision, one audit row, got {audit:?}");
    };
    assert_eq!(row[0], "memd.read_intercepted");
    assert!(
        row[1].starts_with("file:booked.txt@sha256:") && row[1].ends_with(":L1-40"),
        "the handle names the file, the revision and the lines: {row:?}"
    );
    assert_eq!(row[2], "already_held");
    let returned: u64 = row[3].parse().unwrap();
    let avoided: u64 = row[4].parse().unwrap();
    assert!(avoided > 0, "the slice it did not deliver: {row:?}");
    assert!(returned > 0, "the sentence it delivered instead: {row:?}");
    assert!(
        avoided > returned,
        "a decline that cost more than it saved is not a saving: {row:?}"
    );

    let calls = rows(
        &repo,
        &cache,
        "SELECT channel, tool_name, terminal_verb, tokens_returned, tokens_avoided \
         FROM serve_call_event",
    );
    let [call] = calls.as_slice() else {
        panic!("one decision, one call row, got {calls:?}");
    };
    assert_eq!(call[0], "hook_inject", "the channel the decision travelled");
    assert_eq!(
        call[1], "claude-code:read-gate",
        "the family and the step, the only per-family axis these relations have"
    );
    assert_eq!(call[2], "memd.read_intercepted");
    assert_eq!(call[3], row[3], "the same figures, in the other relation");
    assert_eq!(call[4], row[4]);
}

/// ★ **A read Rift caused not to happen is booked, and shows up in the
/// figure.** Driven through the shipped binary, one process per hook:
///
/// 1. the window reads a file, so the journal knows it holds it;
/// 2. the context is compacted and the boundary hands it back the map of what
///    it stopped holding — Rift telling the window, in so many words, that it
///    held this file at this revision and these lines;
/// 3. the window then **edits** that file without reading it again.
///
/// The read at step 3 never happened, and the row that says so lands in the
/// audit relation under its own verb and reaches `rift savings --json` as the
/// prevented part of the avoided total.
#[test]
fn an_announcement_the_window_acts_on_books_a_read_that_never_happened() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    ask_for_recall(&repo);
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("held.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let at = |turn: u32, verb: &str, payload: &str| {
        hook(&repo, "claude-code", verb, "gate-1", turn, payload, &env)
    };

    // 1. the window is handed the file.
    at(1, "PreToolUse", &read_of(&file, 1, 40));
    // 2. it is compacted, and the boundary hands back what it stopped holding.
    at(2, "PreCompact", "{}");
    let map = native_hook(
        &repo,
        "claude-code",
        "SessionStart",
        r#"{"session_id":"gate-1","source":"compact"}"#,
        &env,
    );
    assert!(
        map.contains("held.txt"),
        "the map names the file it says the window held: {map}"
    );
    assert_the_window_records_having_been_told(&repo);

    // 3. the window acts on the file without reading it again.
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line seven\n"}}}}"#,
        json_str(&file)
    );
    at(3, "PreToolUse", &edit);

    let booked = rows(
        &repo,
        &cache,
        "SELECT verb, handle, reason, tokens_returned, tokens_avoided FROM memd_audit_event \
         WHERE verb = 'memd.read_prevented'",
    );
    let [row] = booked.as_slice() else {
        panic!("one act on one announcement, one row, got {booked:?}");
    };
    assert!(
        row[1].starts_with("file:held.txt@sha256:") && row[1].ends_with(":L1-40"),
        "the handle names the file, the revision and the announced lines: {row:?}"
    );
    assert_eq!(row[2], "prevented");
    assert_eq!(row[3], "0", "nothing was put in front of the model");
    let avoided: u64 = row[4].parse().unwrap();
    assert!(avoided > 0, "the read it did not make: {row:?}");

    // And the figure a person reads.
    let (prevented, total) = the_published_savings(&repo, &env);
    assert_eq!(prevented, avoided, "the row, in the published figure");
    assert!(
        prevented > 0 && prevented <= total,
        "inside the avoided total, never beside it: {prevented} of {total}"
    );
}

/// The repository asks for Recall as an operator would: the boundary tiers an
/// announcement travels on are Recall's, so a repository that has not asked
/// for it is never told what it stopped holding.
fn ask_for_recall(repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"gate\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
}

/// The one window's journal holds the line that says it was told what it held
/// — the announcement the next step is an act on.
fn assert_the_window_records_having_been_told(repo: &Path) {
    let names = windows(repo);
    let [only] = names.as_slice() else {
        panic!("one context window, got {names:?}");
    };
    assert!(
        kinds_of(repo, only).contains(&"hold".to_owned()),
        "and the window's journal records having been told: {:?}",
        kinds_of(repo, only)
    );
}

/// The two figures `rift savings --json` publishes about a prevented read:
/// what it says was prevented, and the avoided total that has to contain it.
fn the_published_savings(repo: &Path, env: &[(String, String)]) -> (u64, u64) {
    let out = rift_cmd(repo)
        .args(["savings", "--json"])
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .output()
        .unwrap();
    assert!(out.status.success(), "rift savings failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        json["overall"]["tokens_prevented"].as_u64().unwrap(),
        json["overall"]["tokens_avoided"].as_u64().unwrap(),
    )
}

/// The line floor, spelled as a **number** for the same reason
/// [`SLICE_CEILING`] is.
///
/// A repeat shorter than this is decided and recorded but never declined,
/// because under it the reason frame costs more than the range it replaces.
/// Used here only to say how many of the pool's tokens the floor forgoes — a
/// figure in its own right, so moving the floor shows up as a number rather
/// than as a quiet change in what the share is a share of.
const LINE_FLOOR: u32 = 24;

/// What this repository's own capture measured a re-read to cost, in tokens:
/// 98 re-reads of an unchanged revision over 500,428 tokens, and 58 re-reads
/// of a changed path over 866,705 (first reads, for scale, were 307 over
/// 2,011,810).
///
/// The fixture is sized from these two means rather than from a round number,
/// because the share a reason frame removes depends on the size of
/// the read it replaces: a frame of ~87 tokens against a 160-token read
/// removes barely half of it and against a 5,106-token read removes ~98 %. A
/// fixture of uniform small files therefore measures the frame, not the lever.
const UNCHANGED_REREAD_TOKENS: u32 = 5_106;

/// The mean of the changed class — the read that verifies an edit.
const CHANGED_REREAD_TOKENS: u32 = 14_943;

/// The spread the plain re-reads are drawn from, in tenths of
/// [`UNCHANGED_REREAD_TOKENS`]: eleven multipliers whose mean is exactly the
/// class mean, with a twelfth member sized instead at [`CROSSOVER_LINES`].
const PLAIN_SPREAD: [u32; 11] = [6, 7, 8, 9, 10, 10, 10, 11, 12, 13, 14];

/// The spread the verification reads are drawn from, in tenths of
/// [`CHANGED_REREAD_TOKENS`]: the class mean, one below it and one above, and
/// unbiased on purpose — the three multipliers sum to three times the middle
/// one, so the spread cannot flatter the lever by leaning either way.
///
/// **No member of this fixture reaches [`SLICE_CEILING`]** at the value the
/// ceiling now has: the largest is about 39.8 KB against a ceiling of 96 KiB,
/// and the figures say so by printing `past_ceiling` as zero. The spread is
/// not stretched to reach it, because a member sized to clear a ceiling is a
/// member drawn from no measured class and it would move a published
/// denominator. The ceiling is driven from both sides instead by
/// `a_slice_past_the_ceiling_runs_while_one_under_it_is_declined`, in the
/// deliveries suite, which is where a claim about the ceiling belongs.
const VERIFY_SPREAD: [u32; 3] = [7, 10, 13];

/// The smallest member: the length at which a plain frame stops paying for
/// itself (87 tokens of frame against 4 tokens a line breaks even at 21.75
/// lines). It sits under [`LINE_FLOOR`], so the lever now observes it and
/// lets it run, and its tokens are counted as received like any other member
/// the lever let through. It stays in the fixture for exactly that reason: the
/// floor is a lever that forgoes pool on purpose, and a fixture that dropped
/// the member the floor acts on would stop being able to say what it costs.
const CROSSOVER_LINES: u32 = 22;

/// The file lengths one run of the script uses, in lines, and the row its
/// figures are published under.
struct Sizing {
    /// One length per plain re-read member.
    plain: Vec<u32>,
    /// One length per file that is edited and then read back.
    verified: Vec<u32>,
    label: &'static str,
}

/// How many numbered lines the real tokenizer prices at about `tokens`.
///
/// Derived, never tabulated: bisected with the tokenizer the product prices
/// reads with, so a tokenizer change moves the fixture's sizes with it instead
/// of quietly changing what the fixture claims to measure.
fn lines_for_tokens(tokens: u32) -> u32 {
    let (mut lo, mut hi) = (1_u32, tokens.max(2));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if rift_tokens::count(&numbered(mid, None)) < tokens {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The fixture sized from the measured classes.
///
/// The mapping, stated: each plain re-read is a file the tokenizer prices at
/// its multiplier's tenths of [`UNCHANGED_REREAD_TOKENS`], and each
/// verification read at its multiplier's tenths of [`CHANGED_REREAD_TOKENS`],
/// with one plain member sized instead at [`CROSSOVER_LINES`]. Nothing here is
/// chosen to clear a bar: the changed class's mean member is larger than
/// [`SLICE_CEILING`] and is scripted at its measured size anyway, which is why
/// the figures say how much of the pool the product's own ceiling leaves
/// alone.
fn class_sizes() -> Sizing {
    let tenths = |mean: u32, n: u32| lines_for_tokens(mean * n / 10);
    let mut plain: Vec<u32> = PLAIN_SPREAD
        .iter()
        .map(|n| tenths(UNCHANGED_REREAD_TOKENS, *n))
        .collect();
    plain.push(CROSSOVER_LINES);
    Sizing {
        plain,
        verified: VERIFY_SPREAD
            .iter()
            .map(|n| tenths(CHANGED_REREAD_TOKENS, *n))
            .collect(),
        label: "recall-intercept-savings",
    }
}

/// The same script over uniform forty-line files — the shape the first sitting
/// measured, and now a length just above [`LINE_FLOOR`]. Published beside the
/// class-sized figure so the minimum-slice question keeps its own number, and
/// held to no bar: forty lines is not a size this repository's reads have.
fn small_file_sizes() -> Sizing {
    Sizing {
        plain: vec![40; 12],
        verified: vec![40; 3],
        label: "recall-intercept-savings-small-files",
    }
}

/// What one call would have delivered: the real tokenizer's count of exactly
/// the lines it asked for, and those lines' bytes.
///
/// The slice is cut the way the product cuts it, so this count and the figure
/// the product books for the same key are two independent measurements of one
/// quantity — and the fixture asserts they agree, which is what keeps a price
/// a measurement rather than an estimate. It also prices a member the product
/// never decided about, which a booked figure cannot.
fn price_slice(text: &str, lo: u32, hi: u32) -> (u32, usize) {
    let slice: String = text
        .split_inclusive('\n')
        .skip(lo as usize - 1)
        .take((hi.saturating_sub(lo) as usize) + 1)
        .collect();
    (rift_tokens::count(&slice), slice.len())
}

/// Which measured class a pool member was drawn from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoolClass {
    /// A re-read of bytes the window already holds at the revision it holds.
    Plain,
    /// The read that verifies the agent's own edit.
    Verification,
    /// A later turn asking again for a key this window has declined once.
    NextTurn,
}

/// One call in the script that asked for lines the window already held: a
/// member of the addressable repeat pool, with the lever's answer to it.
struct PoolMember {
    /// The file's name, as a booked handle spells it.
    name: String,
    /// The lines the call asked for, as the journal numbers them.
    lo: u32,
    hi: u32,
    class: PoolClass,
    /// The real tokenizer's count of the slice the call would have delivered:
    /// the tokens the agent would have spent without the lever.
    price: u32,
    /// The bytes of that same slice, which is what [`SLICE_CEILING`] is
    /// spelled in.
    slice_bytes: usize,
    /// Whether the lever left the call alone, so its tokens reached the model.
    escaped: bool,
}

/// The scripted session itself: reads, re-reads, edits with the reads that
/// verify them, and a next turn that comes back to one file.
///
/// Returns EVERY member of the addressable repeat pool, each with the lever's
/// actual answer to it — not only the ones that escaped. A share counted
/// against a denominator the script assembles afterwards is a share of
/// whatever the script chose to admit; this one is counted against what the
/// session asked for.
fn script_the_session(repo: &Path, env: &[(String, String)], sizing: &Sizing) -> Vec<PoolMember> {
    let named = |prefix: &str, lengths: &[u32]| -> Vec<(PathBuf, u32)> {
        lengths
            .iter()
            .enumerate()
            .map(|(n, lines)| (repo.join(format!("{prefix}{n:02}.txt")), *lines))
            .collect()
    };
    let plain = named("p", &sizing.plain);
    let edited = named("e", &sizing.verified);
    for (path, lines) in plain.iter().chain(edited.iter()) {
        std::fs::write(path, numbered(*lines, None)).unwrap();
    }
    let mut turn = 0;

    // Turn one: the agent reads each file. Every one of these is the saving
    // the mechanism exists to protect, and none of them is pool.
    for (path, lines) in plain.iter().chain(edited.iter()) {
        let declined = read_whole(repo, env, &mut turn, path, *lines);
        assert!(!declined, "a first read is never declined");
    }

    // Turn two: it re-reads each of them, unchanged. Pool, every one.
    let mut pool: Vec<PoolMember> = Vec::new();
    for (path, lines) in &plain {
        let declined = read_whole(repo, env, &mut turn, path, *lines);
        let text = numbered(*lines, None);
        pool.push(member(path, &text, *lines, PoolClass::Plain, declined));
    }

    // Turn three: it edits the others and reads each back to check.
    pool.extend(edit_and_verify(repo, env, &mut turn, &edited));

    // The next turn: it comes back to one file it is still working on and
    // asks for it again. The key has been declined once already, so this one
    // runs — pool the mechanism does not remove.
    let (working, lines) = &plain[5];
    let declined = read_whole(repo, env, &mut turn, working, *lines);
    assert!(!declined, "one decline per key: the same call again runs");
    let text = numbered(*lines, None);
    pool.push(member(working, &text, *lines, PoolClass::NextTurn, false));

    pool
}

/// One whole-file read of `path`, in the next turn, and whether the lever
/// declined it.
fn read_whole(
    repo: &Path,
    env: &[(String, String)],
    turn: &mut u32,
    path: &Path,
    lines: u32,
) -> bool {
    *turn += 1;
    let payload = read_of(path, 1, lines + 10);
    let out = hook(
        repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        *turn,
        &payload,
        env,
    );
    declined_with(&out).is_some()
}

/// One call that asked for lines the window already held, priced.
fn member(path: &Path, text: &str, hi: u32, class: PoolClass, declined: bool) -> PoolMember {
    let (price, slice_bytes) = price_slice(text, 1, hi);
    PoolMember {
        name: file_name(path),
        lo: 1,
        hi,
        class,
        price,
        slice_bytes,
        escaped: !declined,
    }
}

/// The edits and the reads that verify them: the largest measured class, and
/// the one this mechanism exists for.
fn edit_and_verify(
    repo: &Path,
    env: &[(String, String)],
    turn: &mut u32,
    files: &[(PathBuf, u32)],
) -> Vec<PoolMember> {
    let mut pool = Vec::new();
    for (path, lines) in files {
        let edit = edit_inserting_line_7b(path);
        let after = numbered(*lines, Some("line 7b"));
        std::fs::write(path, &after).unwrap();
        *turn += 1;
        hook(
            repo,
            "claude-code",
            "PostToolUse",
            "gate-1",
            *turn,
            &edit,
            env,
        );
        let declined = read_whole(repo, env, turn, path, *lines);
        pool.push(member(
            path,
            &after,
            *lines + 1,
            PoolClass::Verification,
            declined,
        ));
    }
    pool
}

/// ★ The scripted session's own numbers: what its addressable repeat pool was
/// worth, and how much of it the lever removed.
///
/// **What the pool is.** Every call in this script that asks for lines the
/// window already holds at the revision it holds them at — the plain re-reads
/// of turn two, the reads that verify the agent's own edits in turn three, and
/// the next turn's re-read of a file it is still working on. Each member is
/// priced by the real tokenizer's count of the slice the call would have
/// delivered, and every member the lever decided about is cross-checked
/// against the figure the product booked for that exact key — two
/// measurements of one quantity, so no price here is estimated.
///
/// **What removed is.** The pool minus what the agent received instead: the
/// reason frames it was answered with, and every pool member the lever let
/// through. It is NOT the avoided column. A mechanism that saved a thousand
/// tokens by spending nine hundred of them on reasons has removed a hundred,
/// and the two figures are published side by side so that neither can stand in
/// for the other. The avoided column is kept because the honesty gate is a
/// statement about it and the frames — avoided greater than returned — which
/// is a different claim from the share.
///
/// **What it is not.** One addressable shape is deliberately outside this
/// fixture: the read that verifies an edit whose sites v1 does not derive
/// (`MultiEdit` and its siblings), which is pool the mechanism does not remove
/// today and which the dogfood window sizes. The slices past
/// `max_slice_bytes` are NOT outside it — the changed class's own mean is one
/// of them — so they are scripted, priced and counted against the mechanism
/// like any other member, with the figures saying how much of the pool they
/// are.
///
/// **The residual that is here.** Two members run untouched, both by design,
/// and both counted against the mechanism rather than excused. One is a
/// next-turn re-read of a key already declined once, because one decline per
/// key is what bounds a model that answers a decline by asking again. The
/// other is the twenty-two-line member, which is under the line floor: the
/// lever reaches the decision, records it and lets the call run, because a
/// reason frame would have cost more than the range. The tokens each forgoes
/// are published as their own figures.
fn measure_the_repeat_pool(sizing: &Sizing) -> Figures {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_at_the_shipped_floor(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let members = script_the_session(&repo, &env, sizing);
    assert_the_pools_composition(&members);

    // The product's own books, by the key each decision was about.
    let booked = rows(
        &repo,
        &cache,
        "SELECT handle, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb = 'memd.read_intercepted'",
    );
    let avoided = cross_check_the_bookings(&members, &booked);
    let returned: u64 = booked.iter().map(|r| r[2].parse::<u64>().unwrap()).sum();
    let figures = figures_for(&members, avoided, returned);
    publish_the_figures(sizing.label, &members, booked.len(), &figures);
    figures
}

/// What the script's own members and the product's books add up to.
///
/// `received` is what the agent got instead of the pool: the reason frames it
/// was answered with, plus every pool member the lever let through — which is
/// why `removed` is a subtraction and never the avoided column.
fn figures_for(members: &[PoolMember], avoided: u64, returned: u64) -> Figures {
    let pool: u64 = members.iter().map(price_of).sum();
    let delivered: u64 = members.iter().filter(|m| m.escaped).map(price_of).sum();
    let received = returned + delivered;
    Figures {
        pool,
        received,
        removed: pool.saturating_sub(received),
        avoided,
        returned,
    }
}

/// The three lines one run of the script publishes: its figures, what the
/// product's own slice ceiling leaves alone, and what the line floor forgoes.
///
/// The last two are printed beside the share and never taken out of it — the
/// bar is held against the whole pool, and these lines say how much of any
/// shortfall is the ceiling or the floor rather than the frames.
fn publish_the_figures(label: &str, members: &[PoolMember], declines: usize, f: &Figures) {
    let &Figures {
        pool,
        received,
        removed,
        avoided,
        returned,
    } = f;
    let delivered: u64 = members.iter().filter(|m| m.escaped).map(price_of).sum();
    let past_ceiling: u64 = members
        .iter()
        .filter(|m| m.slice_bytes > SLICE_CEILING)
        .map(price_of)
        .sum();
    // Pool the lever reached a decision about, recorded, and deliberately did
    // not act on because the reason would have cost more than the range.
    let under_floor: u64 = members
        .iter()
        .filter(|m| m.hi - m.lo + 1 < LINE_FLOOR)
        .map(price_of)
        .sum();

    println!(
        "{label}: pool {pool} · received {received} · removed {removed} · avoided {avoided} · \
         returned {returned} · removed {:.2} % · declines {declines} · pool members {} · \
         escaped {delivered}",
        share(removed, pool),
        members.len(),
    );
    println!(
        "{label}: of that pool {past_ceiling} tokens sit in slices larger than the product's \
         own ceiling of {SLICE_CEILING} bytes, which the lever never acts on; over \
         the rest the fixture removes {:.2} %",
        share(removed, pool - past_ceiling),
    );
    println!(
        "{label}: and {under_floor} tokens sit in repeats shorter than the floor of \
         {LINE_FLOOR} lines, which the lever decides about, records and lets run \
         because a reason would cost more than the range — {:.2} % of the pool, \
         forgone on purpose",
        share(under_floor, pool),
    );
}

/// What one pool member would have cost the agent without the lever.
fn price_of(m: &PoolMember) -> u64 {
    u64::from(m.price)
}

/// `part` as a percentage of `whole`, and zero where there is no whole.
fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

/// The pool's composition, asserted by class: a member dropped from the
/// denominator would raise the share with nothing going red, which is the one
/// way this figure could flatter itself.
fn assert_the_pools_composition(members: &[PoolMember]) {
    let of_class = |class| members.iter().filter(|m| m.class == class).count();
    assert_eq!(
        (
            members.len(),
            of_class(PoolClass::Plain),
            of_class(PoolClass::Verification),
            of_class(PoolClass::NextTurn)
        ),
        (16, 12, 3, 1),
        "the script's own pool: twelve plain repeats, three verification reads, \
         one next-turn repeat"
    );
}

/// Reconcile the script's own pool against the product's books, and return
/// what the lever avoided.
///
/// A declined member must be booked, its booked avoided figure must equal this
/// fixture's own count of the same slice, and its returned figure must be
/// non-zero: a frame the model reads is never free, and booking it free would
/// move the whole cost of this mechanism out of sight. A member the lever let
/// through delivered its price to the model whatever the books say about a key
/// it shares with an earlier decline.
fn cross_check_the_bookings(members: &[PoolMember], booked: &[Vec<String>]) -> u64 {
    let mut avoided = 0_u64;
    for m in members {
        assert!(
            m.price > 0,
            "a member with no price prices nothing: {}",
            m.name
        );
        if m.escaped {
            continue;
        }
        let row = booking_for(booked, m)
            .unwrap_or_else(|| panic!("{} was declined and nothing was booked", m.name));
        assert_eq!(
            row[1].parse::<u64>().unwrap(),
            u64::from(m.price),
            "the product's count of the slice it did not deliver and this fixture's \
             count of the same slice are one quantity: {row:?}"
        );
        assert!(
            row[2].parse::<u64>().unwrap() > 0,
            "a frame the model reads is never free, and booking it free would move \
             the whole cost of this mechanism out of sight: {row:?}"
        );
        avoided += u64::from(m.price);
    }
    assert_eq!(
        booked.len(),
        members.iter().filter(|m| !m.escaped).count(),
        "every booked row is one declined member of this pool and no other"
    );
    avoided
}

/// The one row booked for a member's key, or `None`. Two rows for one key
/// would price it twice, and that is a defect, not a sum.
fn booking_for<'a>(booked: &'a [Vec<String>], m: &PoolMember) -> Option<&'a Vec<String>> {
    let prefix = format!("file:{}@sha256:", m.name);
    let suffix = format!(":L{}-{}", m.lo, m.hi);
    let mut rows = booked
        .iter()
        .filter(|row| row[0].starts_with(&prefix) && row[0].ends_with(&suffix));
    let row = rows.next();
    assert!(
        rows.next().is_none(),
        "two bookings for one key would price {} twice",
        m.name
    );
    row
}

/// The four figures one run of the script publishes.
struct Figures {
    /// The tokens the agent would have spent without the lever.
    pool: u64,
    /// What it received instead: the reason frames, plus every pool member the
    /// lever let through.
    received: u64,
    /// `pool - received`.
    removed: u64,
    /// What the lever avoided, cross-checked against the product's books.
    avoided: u64,
    /// What the frames cost, out of the same rows.
    returned: u64,
}

/// ★ The class-sized fixture is held to the nine-tenths bar.
#[test]
fn the_intercept_fixture_removes_at_least_ninety_percent_of_its_pool() {
    let f = measure_the_repeat_pool(&class_sizes());
    assert!(f.pool > 0, "a fixture with no pool measures nothing");
    assert!(
        f.avoided > f.returned,
        "the honesty gate: what the declines saved must exceed what their \
         reasons cost — avoided {}, returned {}",
        f.avoided,
        f.returned
    );
    assert!(
        f.removed * 100 >= f.pool * 90,
        "the fixture removed {} of a {}-token pool ({:.2} per cent), under the \
         nine-tenths bar; publish this number and raise it as a decision, never \
         tune the fixture",
        f.removed,
        f.pool,
        f.removed as f64 * 100.0 / f.pool as f64
    );
}

/// The same script over uniform forty-line files, published and held to no
/// bar.
///
/// Forty lines is not a size this repository's reads have; the row exists
/// because the minimum-slice question — the length below which a reason frame
/// costs more than the read it replaces — is decided on a number, and this is
/// that number. It sits just above [`LINE_FLOOR`], so every member of it is
/// still declined, and what it publishes is what a decline is worth at the
/// short end: a share far under the class-sized one, for the single reason
/// that the frame is a large fraction of a small read. That is the figure an observe
/// window has to weigh before the floor moves. Only the claims that hold at
/// every size are asserted here.
#[test]
fn the_small_file_fixture_publishes_its_figure_without_a_bar() {
    let f = measure_the_repeat_pool(&small_file_sizes());
    assert!(f.pool > 0, "a fixture with no pool measures nothing");
    assert!(f.received > 0, "the frames the model read are never free");
    assert!(
        f.avoided > f.returned,
        "even at forty lines the declines must save more than their reasons \
         cost — avoided {}, returned {}",
        f.avoided,
        f.returned
    );
}

/// A path's own file name — the spelling a booked handle carries.
fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// ★ An existing repository's explicit `observe` remains authoritative, and a
/// repeated limit-less read still sizes the whole-file pool truthfully.
///
/// The figure is the point. Before the delivery was pinned, such a repeat was
/// priced at the one line it was credited with, about twenty tokens against
/// the thousands the tool actually returns, so an observe window would have
/// reported a mechanism working perfectly at nearly no saving.
#[test]
fn an_explicit_observe_policy_prices_a_limit_less_repeat_as_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let policies = repo.join(".rift/policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"observe\"\nmin_slice_lines = 1\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("observed.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let whole = read_whole_of(&file);

    for turn in [1, 2] {
        assert_eq!(
            declined_with(&hook(
                &repo,
                "claude-code",
                "PreToolUse",
                "gate-1",
                turn,
                &whole,
                &env
            )),
            None,
            "the repository explicitly requested observe"
        );
    }
    assert_eq!(kinds(&repo), vec!["read", "observe"]);

    // The audit row a recorded-but-unacted decision books. It carries no
    // figures by design — nothing was sent to the model, so nothing may be
    // claimed as saved — and what it does carry is the key the pool is sized
    // by: the whole file, not the line the read began on.
    let booked = rows(
        &repo,
        &cache,
        "SELECT handle, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb = 'memd.read_repeat_observed'",
    );
    let [row] = booked.as_slice() else {
        panic!("one decision, one audit row, got {booked:?}");
    };
    assert!(
        row[0].ends_with(":L1-40"),
        "the pool is keyed by the whole file the read holds: {row:?}"
    );
    assert_eq!(row[1], "0", "an unacted decision saved nothing");
    assert_eq!(row[2], "0", "and cost nothing");

    // And the window's own line, which is what the pool report prices: forty
    // lines of it, not the one line an unpinned window would have recorded.
    let names = windows(&repo);
    let observed: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(
            repo.join(".rift")
                .join("state")
                .join("reads")
                .join(&names[0]),
        )
        .unwrap()
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    assert_eq!(observed["k"], "observe");
    assert_eq!(observed["lo"], 1);
    assert_eq!(observed["hi"], 40, "the pool a decline would have removed");
}
