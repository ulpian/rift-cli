//! Argv, rendered into the one bounded surface's own request.
//!
//! Nothing here opens anything: a verb's params object and the grant it asked
//! for are both decided from what the caller typed, which is what lets a
//! malformed call cost nothing.

use std::path::Path;

use rift_recall_ledger::grants::MAX_GRANT_REASON_BYTES;
use rift_recall_types::CrossScopeGrant;
use serde_json::{json, Value};

use crate::cli::{exit, Outcome, RecallCmd};

/// The `(verb, params)` a skill-verb invocation carries. `None` for the three
/// lifecycle subcommands, which [`run`](super::run) has already handled —
/// expressed as an `Option` rather than a panic so the never-guess discipline
/// holds even if a future variant is added and this table is not updated.
pub(super) fn verb_and_params(cmd: &RecallCmd, repo: &Path) -> Option<(&'static str, Value)> {
    use rift_recall_serve::ToolName as T;
    let ranked = |a: &crate::cli::RecallRankedArgs| {
        json!({
            "task": a.task,
            "files_in_focus": focus_paths(repo, &a.files),
            "error_signatures": a.errors,
            "known_hashes": a.known_hashes,
            "token_budget": a.token_budget.unwrap_or(0),
        })
    };
    Some(match cmd {
        RecallCmd::Init | RecallCmd::Status | RecallCmd::Disable => return None,
        RecallCmd::KnownState(a) => (T::KnownState.verb_name(), ranked(a)),
        // Both shapes of one verb: the dispatcher decides which it is from
        // whether a path and a revision are both present, so argv is rendered
        // as it was typed and nothing is inferred here.
        RecallCmd::ChangedHunks(a) => (
            T::ChangedHunks.verb_name(),
            json!({
                "target": a.target,
                "known_hashes": a.known_hashes,
                "task": a.task,
                "files_in_focus": focus_paths(repo, &a.files),
                "error_signatures": a.errors,
                "token_budget": a.token_budget.unwrap_or(0),
            }),
        ),
        RecallCmd::ContextPack(a) => (T::ContextPack.verb_name(), ranked(a)),
        RecallCmd::DiffSince(a) => (
            T::DiffSince.verb_name(),
            json!({
                "known_hashes": a.known_hashes,
                "token_budget": a.token_budget.unwrap_or(0),
            }),
        ),
        RecallCmd::FileIfChanged(a) => (
            T::FileIfChanged.verb_name(),
            json!({ "target": a.target, "known_hashes": a.known_hashes }),
        ),
        RecallCmd::ReadSummary(a) => (
            T::ReadSummary.verb_name(),
            json!({ "target": a.target, "known_hashes": a.known_hashes }),
        ),
        RecallCmd::RetrieveExact(a) => (
            T::RetrieveExact.verb_name(),
            json!({ "target": a.handle, "reason": a.reason }),
        ),
        RecallCmd::CostHotspots(a) => (T::CostHotspots.verb_name(), json!({ "top_n": a.top_n })),
        // Only the path. The document's hash, its declared native cost and the
        // sidecar the family holds for it are RESOLVED, not typed, and they
        // are attached after this decoder rather than through it — a params
        // object that could carry them would let a caller hand the surface the
        // bytes it is asking the surface to serve.
        RecallCmd::DocRead(a) => (T::DocRead.verb_name(), json!({ "target": a.target })),
        // Only the query. Which paths matched, what git says about them and
        // what the lines inside them say are RESOLVED — out of this member's
        // index and work tree — and attached after this decoder rather than
        // through it, for the reason the document verb's are.
        RecallCmd::Search(a) => (T::Search.verb_name(), json!({ "target": a.query })),
    })
}

/// The cross-scope grant this invocation asked for, or the usage
/// refusal that stops it.
///
/// `--cross-branch <REASON>` is how a person deliberately widens containment
/// past this checkout's branch. Two refusals, both **usage** errors, because
/// both are the caller asking wrong rather than the surface saying no:
///
/// * **over [`MAX_GRANT_REASON_BYTES`] bytes.** The bound is in BYTES, not
///   characters, because that is what the column stores and what the writer
///   enforces; a 101-character reason in a two-byte script is over it. Named in
///   the message, so the caller can act on it rather than guess.
/// * **blank.** [`CrossScopeGrant::grant`] answers a blank reason by granting
///   nothing — reason-required by construction — and silently not widening
///   after someone asked to widen is exactly the quiet no-op that teaches
///   people not to trust a flag. Say it instead.
///
/// The writer bounds the text again on its own ([`rift_recall_ledger::grants`]),
/// and that is not redundancy: this refusal is a courtesy to the caller, and the
/// writer's cut is the guarantee about the append-only row.
pub(super) fn cross_branch_grant(cmd: &RecallCmd) -> std::result::Result<CrossScopeGrant, Outcome> {
    let reason = match cmd {
        // Nothing to widen. The three lifecycle subcommands have no scope at
        // all, and neither of the two verbs whose answer is not in the
        // projection is contained by one: a sidecar is keyed by the document's
        // own bytes and is the same file for every member of the family, and a
        // search answers out of the checkout the caller is standing in. No
        // branch axis either way (`ToolName::is_scope_bound`), and a flag with
        // nothing behind it is worse than no flag.
        RecallCmd::Init
        | RecallCmd::Status
        | RecallCmd::Disable
        | RecallCmd::DocRead(_)
        | RecallCmd::Search(_) => None,
        RecallCmd::KnownState(a) | RecallCmd::ContextPack(a) => a.cross_branch.as_deref(),
        RecallCmd::ChangedHunks(a) => a.cross_branch.as_deref(),
        RecallCmd::DiffSince(a) => a.cross_branch.as_deref(),
        RecallCmd::FileIfChanged(a) | RecallCmd::ReadSummary(a) => a.cross_branch.as_deref(),
        RecallCmd::RetrieveExact(a) => a.cross_branch.as_deref(),
        RecallCmd::CostHotspots(a) => a.cross_branch.as_deref(),
    };
    let Some(reason) = reason else {
        return Ok(CrossScopeGrant::none()); // the default: nothing is widened
    };
    // The hint rides the HUMAN line as well as the document, unlike the
    // dispatcher's refusals below: this one is about argv the caller typed, so
    // the person can fix it from what they see. It names a length and a bound,
    // never the reason text itself.
    let usage = |tag: &str, hint: String| {
        Outcome::coded(
            exit::USAGE,
            format!("recall: refused ({tag}) — {hint}"),
            json!({
                "error": tag,
                "detail": { "hint": hint, "max_reason_bytes": MAX_GRANT_REASON_BYTES },
            }),
        )
    };
    if reason.len() > MAX_GRANT_REASON_BYTES {
        return Err(usage(
            "cross_branch_reason_too_long",
            format!(
                "--cross-branch takes at most {MAX_GRANT_REASON_BYTES} bytes; this was {}",
                reason.len()
            ),
        ));
    }
    if reason.trim().is_empty() {
        return Err(usage(
            "cross_branch_reason_required",
            "--cross-branch widens containment and is audited; say why".to_owned(),
        ));
    }
    // Branches only. Widening the developer or repo axis is a different act
    // with a different blast radius, and this flag is not it.
    Ok(CrossScopeGrant::grant(reason, true, false, false))
}

/// The focus set, as repo-relative paths.
///
/// `files_in_focus` is an input to the input-keyed `PackKey`, so an
/// absolute path is not a cosmetic difference: two members of one family asking
/// the identical question about the identical file would compute two keys and
/// miss each other's cache entry — the pack cache would stop compounding at
/// exactly the boundary a shared store exists to cross. A path outside this
/// repository is left alone: it is not ours to rewrite, and it will simply not
/// match anything.
fn focus_paths(repo: &Path, files: &[String]) -> Vec<String> {
    files.iter().map(|f| repo_relative(repo, f)).collect()
}

/// The name a verb argument keys the window journal with: repository-relative
/// and `/`-spelled, whichever base and separator the caller typed it with.
pub(super) fn repo_relative(repo: &Path, path: &str) -> String {
    use rift_recall_substrate::worktree::repo_relative_slash;
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    let candidate = Path::new(trimmed);
    for base in [Some(repo.to_path_buf()), repo.canonicalize().ok()]
        .into_iter()
        .flatten()
    {
        if candidate.strip_prefix(&base).is_ok() {
            return repo_relative_slash(&base, candidate).unwrap_or_default();
        }
    }
    repo_relative_slash(Path::new(""), candidate).unwrap_or_else(|| trimmed.to_owned())
}

/// Whether this invocation asked for matching lines as well as ranked paths.
///
/// Read off argv rather than off the params object, because the params object
/// is the decoder's and the decoder never sets the search input: `--content`
/// decides which native call the answer stands in for, and so which baseline it
/// may claim, which is a decision the resolver makes and not the surface.
pub(super) fn search_content(cmd: &RecallCmd) -> bool {
    matches!(cmd, RecallCmd::Search(a) if a.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use rift_recall_serve::{decode_verb, ToolName, TOOL_SURFACE};

    use crate::recall::tests::{argv_for, parse};

    /// The anti-drift property that makes this surface trustworthy: clap's
    /// subcommand spelling, the dispatcher's verb spelling, and the tool the
    /// call lands on are the same closed set of eight. A tool added to
    /// `TOOL_SURFACE` without a CLI verb (or vice versa) fails here rather
    /// than shipping a surface that advertises a command that does not run.
    #[test]
    fn every_dispatcher_tool_has_a_clap_verb_that_decodes_back_to_it() {
        for tool in TOOL_SURFACE.iter().copied() {
            let cmd = parse(&argv_for(tool));
            let (verb, params) = verb_and_params(&cmd, Path::new("/repo")).expect("a skill verb");
            assert_eq!(verb, tool.verb_name());
            let (decoded, _req) = decode_verb(verb, &params).expect("the verb decodes");
            assert_eq!(decoded, tool, "clap verb {verb} must land on {tool:?}");
        }
    }

    /// The lifecycle subcommands are terminal-owned, not part of the agent's
    /// bounded surface — they must never be routed through the dispatcher.
    #[test]
    fn lifecycle_subcommands_are_not_skill_verbs() {
        for argv in [vec!["init"], vec!["status"], vec!["disable"]] {
            assert!(
                verb_and_params(&parse(&argv), Path::new("/repo")).is_none(),
                "{argv:?}"
            );
        }
    }

    /// argv reaches the dispatcher verbatim: repeated flags accumulate, and an
    /// absent `--token-budget` means "unbudgeted" (0), never a silent default
    /// the agent did not ask for.
    #[test]
    fn repeated_flags_accumulate_and_absent_budget_is_zero() {
        let cmd = parse(&[
            "known-state",
            "--task",
            "fix the gate",
            "--file",
            "a.rs",
            "--file",
            "b.rs",
            "--error",
            "E0432",
            "--known-hash",
            "aa",
            "--known-hash",
            "bb",
        ]);
        let (_verb, params) = verb_and_params(&cmd, Path::new("/repo")).unwrap();
        assert_eq!(
            params["files_in_focus"],
            serde_json::json!(["a.rs", "b.rs"])
        );
        assert_eq!(params["error_signatures"], serde_json::json!(["E0432"]));
        assert_eq!(params["known_hashes"], serde_json::json!(["aa", "bb"]));
        assert_eq!(params["token_budget"], serde_json::json!(0));
    }

    /// The focus set reaches `PackKey` repo-relative, whichever spelling the
    /// caller used. Two members of one family asking the same question about
    /// the same file must compute the same key, or the pack cache stops
    /// compounding exactly where a shared store is supposed to start.
    #[test]
    fn focus_paths_are_normalised_to_repo_relative() {
        let repo = std::path::Path::new("/fam/main");
        assert_eq!(
            focus_paths(
                repo,
                &[
                    "/fam/main/src/a.rs".to_owned(),
                    "./src/b.rs".to_owned(),
                    "src/c.rs".to_owned(),
                    "/elsewhere/d.rs".to_owned(),
                ]
            ),
            vec![
                "src/a.rs".to_owned(),
                "src/b.rs".to_owned(),
                "src/c.rs".to_owned(),
                // Not ours to rewrite; it will simply match nothing.
                "/elsewhere/d.rs".to_owned(),
            ]
        );
    }

    /// The flag reaches the grant on **every** verb, through the production
    /// argv path — clap parse, then the same function `serve` calls.
    ///
    /// Per-verb rather than once, because the argument lives on five separate
    /// clap structs: a verb whose struct missed the field would advertise a
    /// flag that parses nowhere, or parse one that widens nothing.
    ///
    /// Every verb a scope contains, which is every verb whose answer comes out
    /// of the projection. The document verb is not one: a sidecar is keyed by
    /// the document's own bytes and is the same file for every member of the
    /// family, so there is no branch axis to widen — and a flag with nothing
    /// behind it is exactly what this test exists to prevent.
    #[test]
    fn the_cross_branch_flag_reaches_the_grant_on_every_scoped_verb() {
        for tool in TOOL_SURFACE.iter().copied().filter(|t| t.is_scope_bound()) {
            let mut argv = argv_for(tool);
            argv.push("--cross-branch");
            argv.push("reviewing the sibling worktree");
            let cmd = parse(&argv);
            let grant = cross_branch_grant(&cmd).expect("a real reason is accepted");
            assert!(grant.branches(), "{tool:?} did not widen the branch axis");
            assert_eq!(grant.reason(), Some("reviewing the sibling worktree"));
            assert!(
                !grant.developers() && !grant.repos(),
                "{tool:?}: --cross-branch widens ONE axis"
            );
        }
    }

    /// Absent flag, no grant — which is what makes the audited tag mean
    /// something when it is there.
    #[test]
    fn an_absent_cross_branch_flag_builds_no_grant() {
        for tool in TOOL_SURFACE.iter().copied() {
            let grant = cross_branch_grant(&parse(&argv_for(tool))).expect("no flag, no refusal");
            assert!(!grant.is_any(), "{tool:?}");
            assert!(grant.reason().is_none());
        }
        // The lifecycle subcommands have no scope to widen at all.
        for argv in [vec!["init"], vec!["status"], vec!["disable"]] {
            assert!(!cross_branch_grant(&parse(&argv)).unwrap().is_any());
        }
    }

    /// The bound is in BYTES and the refusal says so — a usage error, decided
    /// from argv before a store is opened.
    #[test]
    fn a_cross_branch_reason_over_the_byte_bound_is_a_usage_error() {
        let over = "a".repeat(MAX_GRANT_REASON_BYTES + 1);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            over.as_str(),
        ];
        let refusal = cross_branch_grant(&parse(&argv)).expect_err("over the bound is refused");
        assert_eq!(refusal.code, exit::USAGE);
        assert_eq!(refusal.json["error"], "cross_branch_reason_too_long");
        assert_eq!(
            refusal.json["detail"]["max_reason_bytes"],
            serde_json::json!(MAX_GRANT_REASON_BYTES),
            "the refusal names the bound"
        );
        assert!(refusal.human.contains(&MAX_GRANT_REASON_BYTES.to_string()));

        // Exactly at the bound is accepted: the refusal is `>`, not `>=`.
        let at = "a".repeat(MAX_GRANT_REASON_BYTES);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            at.as_str(),
        ];
        assert!(cross_branch_grant(&parse(&argv)).unwrap().branches());

        // BYTES, not characters: 101 two-byte characters is 202 bytes and over
        // a 200-byte bound, while being well under it counted as characters.
        let wide = "é".repeat(MAX_GRANT_REASON_BYTES / 2 + 1);
        assert!(wide.chars().count() <= MAX_GRANT_REASON_BYTES);
        assert!(wide.len() > MAX_GRANT_REASON_BYTES);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            wide.as_str(),
        ];
        let refusal = cross_branch_grant(&parse(&argv)).expect_err("bytes, not characters");
        assert_eq!(refusal.json["error"], "cross_branch_reason_too_long");
    }

    /// A blank reason is said out loud rather than silently granting nothing.
    #[test]
    fn a_blank_cross_branch_reason_is_refused_rather_than_swallowed() {
        for blank in ["", "   ", "\t"] {
            let argv = vec!["diff-since", "--cross-branch", blank];
            let refusal = cross_branch_grant(&parse(&argv)).expect_err("blank is refused");
            assert_eq!(refusal.code, exit::USAGE);
            assert_eq!(refusal.json["error"], "cross_branch_reason_required");
        }
    }

    /// The two spellings are ONE verb, and this is where that is checkable.
    ///
    /// `rift doc read <path>` is an alias, not a second command: it parses into
    /// the same arguments the long spelling parses into and routes through the
    /// same function, so a change to one cannot leave the other behind. The
    /// census records it `false` — guidance names the taught spelling — and
    /// this is the assertion that the untaught spelling still answers the same
    /// thing.
    #[test]
    fn the_document_alias_and_the_long_spelling_are_one_verb() {
        let long = parse(&["doc-read", "docs/plan.docx"]);
        let short = match crate::cli::Cli::try_parse_from(["rift", "doc", "read", "docs/plan.docx"])
            .expect("the alias parses")
            .cmd
        {
            Some(crate::cli::Command::Doc(crate::cli::DocCmd::Read(a))) => {
                crate::cli::RecallCmd::DocRead(a)
            }
            other => panic!("expected `doc read`, got {other:?}"),
        };
        let here = Path::new("/repo");
        let (verb, params) = verb_and_params(&long, here).expect("a skill verb");
        assert_eq!(verb, ToolName::DocRead.verb_name());
        assert_eq!(
            verb_and_params(&short, here),
            Some((verb, params)),
            "the alias must reach the same verb with the same params"
        );
    }

    /// The two spellings are ONE verb, and this is where that is checkable.
    ///
    /// `rift search <query>` is an alias, not a second command: it parses into
    /// the same arguments the long spelling parses into and routes through the
    /// same function, so a change to one cannot leave the other behind. The
    /// census records it `false` — guidance names the taught spelling — and
    /// this is the assertion that the untaught spelling still answers the same
    /// thing, `--content` included.
    #[test]
    fn the_search_alias_and_the_long_spelling_are_one_verb() {
        for argv in [
            vec!["search", "read gate"],
            vec!["search", "read gate", "--content"],
        ] {
            let long = parse(&argv);
            let mut short_argv = vec!["rift"];
            short_argv.extend_from_slice(&argv);
            let short = match crate::cli::Cli::try_parse_from(short_argv)
                .expect("the alias parses")
                .cmd
            {
                Some(crate::cli::Command::Search(a)) => crate::cli::RecallCmd::Search(a),
                other => panic!("expected `search`, got {other:?}"),
            };
            let here = Path::new("/repo");
            let (verb, params) = verb_and_params(&long, here).expect("a skill verb");
            assert_eq!(verb, ToolName::Search.verb_name());
            assert_eq!(
                verb_and_params(&short, here),
                Some((verb, params)),
                "the alias must reach the same verb with the same params"
            );
            assert_eq!(
                search_content(&short),
                search_content(&long),
                "and carry the same content decision"
            );
        }
    }

    /// The verb takes no `--cross-branch`, and that is a routing fact rather
    /// than a missing flag: its answer is this checkout's own file list, which
    /// no branch axis contains.
    #[test]
    fn the_search_verb_has_no_scope_to_widen() {
        let cmd = parse(&["search", "read gate"]);
        let grant = cross_branch_grant(&cmd).unwrap_or_else(|_| panic!("no flag, no refusal"));
        assert!(!grant.branches());
        assert!(!rift_recall_serve::ToolName::Search.is_scope_bound());
        assert!(!search_content(&cmd));
        assert!(search_content(&parse(&[
            "search",
            "read gate",
            "--content"
        ])));
    }
}
