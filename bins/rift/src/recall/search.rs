//! The search verb's half of a request, resolved out of this member's own
//! path index and work tree.
//!
//! A separate module because the answer is not in the projection: it is this
//! checkout's files, ranked with facts gathered here so the engine stays a
//! pure function of its arguments.

use std::path::Path;

use rift_store::SqliteStore;
use serde_json::json;

use crate::cli::{exit, Outcome};

/// Resolve the search half of a `search` call: what this member's path index
/// answers for the query, the lines inside the paths it ranked when the caller
/// asked for them, and the declared cost of the listing that answer replaces —
/// or the refusal that stops the call.
///
/// **Nothing walks past the cold budget here.** An index that is missing or
/// stale is rebuilt inline only while a bounded walk fits inside
/// [`COLD_BUDGET`](rift_recall_search::COLD_BUDGET); past it the call is
/// answered `pending`, a rebuild is asked of the detached worker, and the next
/// call is answered from a file. That is the same rule the hook arms are held
/// to, and it is why this verb is safe to call on any repository.
///
/// The facts the ranking needs are gathered *here* rather than in the engine,
/// which is what keeps the engine a pure function of its arguments: the window
/// journals of every member of the family (so a file a sibling worktree opened
/// this morning is warm here this afternoon), the projection's own read counts,
/// and what git says is in flight.
#[cfg(feature = "search")]
pub(super) async fn resolve_search(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    content: bool,
    repo: &Path,
    cfg: &rift_config::EffectiveConfig,
    store: &SqliteStore,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    use rift_recall_search::{engine::PathQuery, SearchEngine as _};
    use rift_recall_serve::{IndexState, Search, SearchPath, ToolName, NATIVE_LISTING_PATHS};

    if tool != ToolName::Search {
        return Ok(req);
    }
    let query = req.target.clone().unwrap_or_default();
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    if let Some(refusal) = search_refusal_before_the_engine(&query, &policy) {
        return Err(refusal);
    }
    // Nothing to rank from, and no budget to walk one: the rebuild is asked of
    // the detached worker and this call is answered `pending`, which the next
    // one answers from a file.
    let Some(index) = path_index_for_this_member(repo, cfg) else {
        return Ok(rift_recall_serve::ToolRequest {
            search: Some(Search {
                content,
                declared_baseline: 0,
                index: IndexState::Pending,
            }),
            ..req
        });
    };
    let engine = ranking_engine_over_this_member(index, repo, store).await;
    let ranked = engine.rank_paths(&PathQuery {
        text: query.clone(),
        // The native listing's own cap, because that is the set the declared
        // baseline prices: ranking fewer would claim a listing nobody would
        // have received, and ranking more would price one nobody would either.
        limit: NATIVE_LISTING_PATHS,
        now_ms: u64::try_from(crate::hooks::now_ms()).unwrap_or(0),
    });
    let paths: Vec<String> = ranked.hits.iter().map(|hit| hit.path.clone()).collect();
    let lines = if content {
        matched_lines(&engine, repo, &query, &paths, policy.search.grep_head_limit)
    } else {
        Vec::new()
    };
    let hits = ranked
        .hits
        .iter()
        .map(|hit| SearchPath {
            path: hit.path.clone(),
            status: git_word(engine.status(), &hit.path),
        })
        .collect();
    Ok(rift_recall_serve::ToolRequest {
        search: Some(Search {
            content,
            declared_baseline: priced_search_baseline(cfg, &policy, &paths, content),
            index: IndexState::Answered {
                hits,
                total: ranked.total,
                lines,
            },
        }),
        ..req
    })
}

/// The two refusals this verb decides before the engine runs: a query with
/// nothing in it, and a repository that has switched search off.
///
/// The second is the owner's one kill switch for this plane, and it has to
/// close the verb as well as the hook arms — a lever that stays reachable
/// through a second door is not switched off.
#[cfg(feature = "search")]
fn search_refusal_before_the_engine(
    query: &str,
    policy: &rift_recall_policy::ServePolicy,
) -> Option<Outcome> {
    if query.trim().is_empty() {
        return Some(Outcome::coded(
            exit::USAGE,
            "search: refused (empty_query) — say what to look for".to_owned(),
            json!({ "verb": "search", "error": "empty_query" }),
        ));
    }
    if policy.search.mode == rift_recall_policy::Mode::Off {
        return Some(Outcome::coded(
            exit::PRECONDITION,
            "search: search is switched off for this repository ([search] mode)".to_owned(),
            json!({ "verb": "search", "error": "search_off" }),
        ));
    }
    None
}

/// This checkout's path index, or `None` when there was neither a saved one
/// nor the budget to walk one — in which case a rebuild is owed.
#[cfg(feature = "search")]
fn path_index_for_this_member(
    repo: &Path,
    cfg: &rift_config::EffectiveConfig,
) -> Option<rift_recall_search::PathIndex> {
    use rift_recall_search::{open_for, Opened, COLD_BUDGET};

    let head = rift_recall_search::head_oid(repo);
    match open_for(repo, &head, cfg.sensitive_paths(), COLD_BUDGET) {
        Opened::Fresh(index) => Some(index),
        // Built inside the budget and worth keeping: the caller here is not on
        // a hook's clock, and the next one — which may be — finds a file rather
        // than a walk. Best-effort; a checkout Rift cannot write to still got
        // its answer.
        Opened::Cold(index) => {
            let _ = index.save(&rift_recall_search::index_path(repo));
            Some(index)
        }
        Opened::Absent => {
            owe_search_index(repo);
            None
        }
    }
}

/// The engine, with the facts the ranking needs gathered around it.
///
/// Gathered here rather than in the engine, which is what keeps the engine a
/// pure function of its arguments: the window journals of every member of the
/// family, the projection's own read counts, and what git says is in flight.
#[cfg(feature = "search")]
async fn ranking_engine_over_this_member(
    index: rift_recall_search::PathIndex,
    repo: &Path,
    store: &SqliteStore,
) -> rift_recall_search::NativeEngine {
    use rift_recall_search::{FrecencyFacts, NativeEngine, StatusFacts};

    let status = StatusFacts::collect(repo);
    let mut frecency = FrecencyFacts::from_members(&family_work_trees(repo));
    frecency.absorb_read_counts(family_read_counts(store).await);
    NativeEngine::new(index)
        .with_frecency(frecency)
        .with_status(status)
}

/// What this answer may claim it stood in for.
///
/// The ranked head minus the paths the chokepoint will refuse to render: a
/// path Rift will not name is not one it may bill for either. The screen runs
/// here rather than after the answer so that the two can never differ.
///
/// Priced as a glob and never as a shell listing: this verb is nobody's tool
/// call. It is a fuzzy query an operator typed, it has no client result
/// ceiling of its own, and the smaller of the two bounds is the one a figure
/// Rift will claim credit for has to take. The third screen is empty on
/// purpose — an operator who asked this verb for a name is shown the name, so
/// the repository's sensitive list is not a screen here, and a path this verb
/// WILL say out loud is one it may also price.
#[cfg(feature = "search")]
fn priced_search_baseline(
    cfg: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    paths: &[String],
    content: bool,
) -> u32 {
    let priced = rift_recall_serve::priceable_listing(
        &rift_recall_policy::PolicyEngine::new(cfg).with_serve_policy(policy.clone()),
        rift_recall_serve::ListingShape::Glob,
        rift_recall_serve::NO_CLIENT_FAMILY,
        paths,
        |_| false,
    );
    rift_recall_serve::declared_search_baseline(
        content,
        rift_recall_serve::ListingShape::Glob,
        rift_recall_serve::NO_CLIENT_FAMILY,
        &priced,
    )
}

/// The lines the caller asked for, inside the paths that ranked.
///
/// A separate pass, and separately budgeted, because it is a different cost and
/// a different risk: ranking touches no file content at all, and this reads
/// bytes an agent may then be shown. The cap is the one this repository already
/// declares for a content search that carried none of its own — one knob, not
/// two — and the engine's own screens decide which of the ranked paths may be
/// opened at all.
#[cfg(feature = "search")]
fn matched_lines(
    engine: &rift_recall_search::NativeEngine,
    repo: &Path,
    query: &str,
    paths: &[String],
    head_limit: u32,
) -> Vec<rift_recall_serve::SearchLine> {
    use rift_recall_search::{content::ContentQuery, query::literal_needle, SearchEngine as _};

    engine
        .content_lines(
            repo,
            &ContentQuery {
                needle: literal_needle(query),
                ignore_case: true,
                paths: paths.to_vec(),
                max_hits: head_limit as usize,
                ..ContentQuery::default()
            },
        )
        .into_iter()
        .map(|hit| rift_recall_serve::SearchLine {
            path: hit.path,
            line: u32::try_from(hit.line_number).unwrap_or(u32::MAX),
            text: hit.line,
        })
        .collect()
}

/// The same call in a build without the search seam.
///
/// A refusal rather than a `pending`, and the difference matters: `pending`
/// promises a rebuild the worker will do, and this build has nothing to do it
/// with. Saying which of the two it is costs a line and saves an agent a
/// second call that would answer the same.
#[cfg(not(feature = "search"))]
#[allow(clippy::unused_async)] // one signature for both builds
pub(super) async fn resolve_search(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    _content: bool,
    _repo: &Path,
    _cfg: &rift_config::EffectiveConfig,
    _store: &SqliteStore,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    if tool != rift_recall_serve::ToolName::Search {
        return Ok(req);
    }
    Err(Outcome::coded(
        exit::PRECONDITION,
        "search: this build has no search engine".to_owned(),
        json!({ "verb": "search", "error": "search_not_in_this_build" }),
    ))
}

/// What git says about one ranked path, as the closed vocabulary the renderer
/// and the answer both read.
///
/// Untracked first, then the working tree, then the index: a file that is both
/// modified and staged is reported modified, because the working tree is the
/// newer of the two facts and the one the developer is looking at.
#[cfg(feature = "search")]
fn git_word(status: &rift_recall_search::StatusFacts, path: &str) -> rift_recall_serve::PathStatus {
    use rift_recall_serve::PathStatus;

    if status.is_untracked(path) {
        PathStatus::Untracked
    } else if status.modified.contains(path) {
        PathStatus::Modified
    } else if status.staged.contains(path) {
        PathStatus::Staged
    } else {
        PathStatus::Tracked
    }
}

/// Every checkout of this repository family, this one included.
///
/// The window journals are per member and the interest they record is the
/// family's, so a file a sibling worktree opened this morning ranks warm here
/// this afternoon. A repository that cannot be enumerated contributes this
/// checkout alone, which ranks colder rather than wrongly.
#[cfg(feature = "search")]
fn family_work_trees(repo: &Path) -> Vec<std::path::PathBuf> {
    let Ok(paths) = rift_git::discover(repo) else {
        return vec![repo.to_path_buf()];
    };
    let mut trees: Vec<std::path::PathBuf> = rift_git::family_members(&paths)
        .into_iter()
        .map(|member| member.work_tree)
        .collect();
    if !trees.iter().any(|tree| tree == repo) {
        trees.push(repo.to_path_buf());
    }
    trees
}

/// How many times the projection has recorded a read of each path.
///
/// The half of frecency the window journals cannot see: reads ingested from
/// checkpoints this checkout's own run state never held. They carry no time of
/// their own — they are the whole history rather than an event — so they add
/// accesses and leave recency to the window journals. Bounded, and read
/// best-effort: a projection that will not answer ranks every path colder,
/// never wrongly.
#[cfg(feature = "search")]
async fn family_read_counts(store: &SqliteStore) -> Vec<(String, u32)> {
    use rift_store::CanonicalStore as _;

    store
        .query(
            "SELECT r.path, COUNT(*) AS n FROM n_file_read fr \
             JOIN n_file_revision r ON r.node_id = fr.file_revision \
             GROUP BY r.path ORDER BY n DESC LIMIT 5000",
            &[],
        )
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            let path = row.get_text(0).ok()?;
            let count = row.get_i64(1).ok()?;
            Some((path, u32::try_from(count).unwrap_or(u32::MAX)))
        })
        .collect()
}

/// Ask for a rebuild of this checkout's path index.
///
/// A request, not the work, and the same one the session-start and post-commit
/// hooks make: whichever of them asks first, the worker walks once. Best-effort
/// — an answer already given is never retracted because a job file could not be
/// written.
#[cfg(feature = "search")]
fn owe_search_index(repo: &Path) -> bool {
    let Ok(paths) = rift_git::discover(repo) else {
        return false;
    };
    crate::hooks::owe_search_index(&paths.git_dir, &paths.work_tree)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "search")]
    use super::*;

    /// A checkout with a couple of files in it and no `.rift` policy: enough to
    /// drive every decision this resolver makes, and nothing it does not.
    ///
    /// **Its index is built before any call, by the detached worker's own
    /// pass.** These tests are about the answer, not the cold budget: left to
    /// walk inline, the resolver keeps a walk only while it fits the
    /// wall-clock [`COLD_BUDGET`](rift_recall_search::COLD_BUDGET), so a
    /// loaded machine answered even a two-file checkout `pending`. A saved
    /// index that answers is loaded whatever the clock says. What the budget
    /// decides is held, with an injected budget, by
    /// `rift_recall_search`'s `a_walk_that_cannot_fit_the_budget_is_not_started`.
    #[cfg(feature = "search")]
    fn search_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp repo");
        for (rel, body) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        index_as_the_worker_does(dir.path());
        dir
    }

    /// The rebuild the detached worker runs (`hooks::worker`), with the
    /// sensitive paths of the same config the resolver reads, so the index on
    /// disk is the one production would have left there.
    #[cfg(feature = "search")]
    fn index_as_the_worker_does(repo: &Path) {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        let head = rift_recall_search::head_oid(repo);
        rift_recall_search::rebuild(repo, &head, cfg.sensitive_paths())
            .expect("the worker's own pass");
    }

    #[cfg(feature = "search")]
    fn search_request(query: &str) -> rift_recall_serve::ToolRequest {
        let (_tool, req) = rift_recall_serve::decode_verb("search", &json!({ "target": query }))
            .expect("the verb decodes");
        req
    }

    #[cfg(feature = "search")]
    fn resolve_search_in(
        repo: &Path,
        query: &str,
        content: bool,
    ) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        let store = rift_store::SqliteStore::open_in_memory().expect("a projection");
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).expect("migrated");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        rt.block_on(resolve_search(
            rift_recall_serve::ToolName::Search,
            search_request(query),
            content,
            repo,
            &cfg,
            &store,
        ))
    }

    /// The resolver hands the surface a ranked answer over this checkout's own
    /// files, and prices the listing it stands in for.
    ///
    /// What is asserted here is the join the surface cannot see: the engine ran
    /// over the member's index, the paths came back best-first for the query,
    /// the total is the whole match set rather than the served prefix, and the
    /// declared baseline is a real figure over those same paths.
    #[cfg(feature = "search")]
    #[test]
    fn a_search_resolves_this_checkouts_own_paths_and_prices_the_listing() {
        let dir = search_repo(&[
            ("bins/rift/src/hooks.rs", "fn read_gate() {}\n"),
            ("crates/rift-recall-serve/src/lib.rs", "fn dispatch() {}\n"),
            ("README.md", "# rift\n"),
        ]);
        let req = resolve_search_in(dir.path(), "hooks", false).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        assert!(!search.content);
        let rift_recall_serve::IndexState::Answered { hits, total, lines } = &search.index else {
            panic!("an indexed checkout is not pending");
        };
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some("bins/rift/src/hooks.rs"),
            "the query's own word ranks first: {hits:?}"
        );
        assert!(lines.is_empty(), "no content was asked for");
        assert!(*total >= 1);
        assert!(
            search.declared_baseline > 0,
            "a listing of real paths is not free"
        );
        // The query travels as the target and nothing else: the ranked answer
        // is resolved, never typed.
        assert_eq!(req.target.as_deref(), Some("hooks"));
    }

    /// `--content` reads the lines out of the files that ranked, and the answer
    /// claims nothing for them.
    ///
    /// **Scoped to the ranked paths, deliberately, and it is a real limit.** A
    /// content search here is "of the files this query is about, which lines
    /// match", so a literal that appears in no path is not found — where a
    /// native grep would have walked the whole tree. That is a narrower answer,
    /// never a wrong one, and narrowing is the direction this plane errs in:
    /// the caller still has its own grep, and this verb never claims to have
    /// replaced one — it declares no baseline for a content call at all.
    #[cfg(feature = "search")]
    #[test]
    fn a_content_search_resolves_the_matching_lines_and_declares_no_baseline() {
        let dir = search_repo(&[
            (
                "bins/rift/src/hooks.rs",
                "fn hooks_gate() {}\nfn other() {}\n",
            ),
            ("README.md", "# rift\n"),
        ]);
        let req = resolve_search_in(dir.path(), "hooks", true).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        assert!(search.content);
        assert_eq!(
            search.declared_baseline, 0,
            "a content search has no declared native cost"
        );
        let rift_recall_serve::IndexState::Answered { lines, .. } = &search.index else {
            panic!("an indexed checkout is not pending");
        };
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = lines.first().expect("one line");
        assert_eq!(line.path, "bins/rift/src/hooks.rs");
        assert_eq!(line.line, 1);
        assert_eq!(line.text, "fn hooks_gate() {}");

        // The limit, stated as a test rather than as a sentence: a literal no
        // ranked path is about is not found, and the answer is empty rather
        // than a walk of the tree.
        let elsewhere = resolve_search_in(dir.path(), "readme", true).expect("a resolved search");
        let rift_recall_serve::IndexState::Answered { lines, .. } = &elsewhere
            .search
            .as_ref()
            .expect("the resolver attached one")
            .index
        else {
            panic!("an indexed checkout is not pending");
        };
        assert!(
            lines.iter().all(|line| line.text != "fn other() {}"),
            "a content search is scoped to the paths the query ranked: {lines:?}"
        );
    }

    /// Two refusals, both decided before the engine runs: a query with nothing
    /// in it, and a repository that has switched search off.
    ///
    /// The second is the owner's one kill switch for this plane, and it has to
    /// close the verb as well as the hook arms — a lever that stays reachable
    /// through a second door is not switched off.
    #[cfg(feature = "search")]
    #[test]
    fn an_empty_query_and_a_repository_with_search_off_are_refused() {
        let dir = search_repo(&[("README.md", "# rift\n")]);
        let blank = resolve_search_in(dir.path(), "   ", false).expect_err("blank is refused");
        assert_eq!(blank.code, exit::USAGE);
        assert_eq!(blank.json["error"], "empty_query");

        let policies = dir.path().join(".rift/policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("serve.toml"), "[search]\nmode = \"off\"\n").unwrap();
        let off = resolve_search_in(dir.path(), "hooks", false).expect_err("switched off");
        assert_eq!(off.code, exit::PRECONDITION);
        assert_eq!(off.json["error"], "search_off");
    }

    /// A sensitive path is not in the index at all, so it cannot be ranked,
    /// named or grepped by anything downstream.
    ///
    /// The surface screens the deny list a second time; this is the screen
    /// *before* it, and it is the reason the second one has nothing to do in
    /// the ordinary case. The fixture asserts its own premise so it cannot pass
    /// because a built-in glob moved.
    #[cfg(feature = "search")]
    #[test]
    fn a_sensitive_path_never_enters_the_index_a_search_ranks() {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        assert!(
            cfg.is_sensitive_path("config/.env.production"),
            "the fixture's premise: the shipped config treats this path as sensitive"
        );
        let dir = search_repo(&[
            ("config/.env.production", "STRIPE_KEY=live\n"),
            ("config/settings.rs", "const A: u8 = 1;\n"),
        ]);
        let req = resolve_search_in(dir.path(), "config", true).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        let rift_recall_serve::IndexState::Answered { hits, lines, .. } = &search.index else {
            panic!("an indexed checkout is not pending");
        };
        assert!(
            hits.iter().all(|hit| hit.path != "config/.env.production"),
            "{hits:?}"
        );
        assert!(lines.iter().all(|line| line.text != "STRIPE_KEY=live"));
    }
}
