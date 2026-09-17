//! The palette's suggestion layer: catalog, ranking, and
//! context-sensitive argument completion. Pure functions of the buffer, the
//! completion context, and the in-memory usage log — deterministic, no I/O,
//! no clock. The animated panel that renders these rows is `view.rs`'s; this
//! module deliberately takes a name distinct from the colour
//! `palette` module.

/// What one verb's argument slot completes to (drives the context table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// No arguments.
    None,
    /// A registered project (select / remove / relabel).
    Project,
    /// A panel name (focus).
    Panel,
    /// A memory subcommand.
    MemorySub,
    /// A checkpoint subcommand.
    CheckpointSub,
    /// A session subcommand.
    SessionSub,
    /// A projects subcommand.
    ProjectsSub,
    /// A profile subcommand (new / rename / switch / delete).
    ProfileSub,
    /// A splash mode (configure splash).
    ConfigureSub,
    /// `--dry-run` (gc).
    GcFlag,
    /// A free-form id / reference (explain, show).
    Id,
    /// An agent token (enable / disable).
    Agent,
}

/// One catalog entry: exactly one per grammar verb word (proven in sync by
/// test — help text can never advertise a verb that does not parse).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The verb word (the buffer's first token).
    pub verb: &'static str,
    /// One-line usage.
    pub usage: &'static str,
    /// One-line description (muted in the panel).
    pub description: &'static str,
    /// What the first argument completes to.
    pub arg: ArgKind,
}

/// The static catalog — the `/help` body and the suggest panel's source.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        verb: "help",
        usage: "/help",
        description: "show every palette verb",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "status",
        usage: "/status",
        description: "capture / projection health snapshot",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "select",
        usage: "/select [project]",
        description: "select a project row",
        arg: ArgKind::Project,
    },
    CatalogEntry {
        verb: "settings",
        usage: "/settings",
        description: "open repository Project Settings",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "viewer",
        usage: "/viewer [target] [--repo path]",
        description: "open the read-only checkpoint and Recall viewer",
        arg: ArgKind::Id,
    },
    CatalogEntry {
        verb: "memory",
        usage: "/memory <verb>",
        description: "ask the recall plane (diffs, hotspots, rereads)",
        arg: ArgKind::MemorySub,
    },
    CatalogEntry {
        verb: "doctor",
        usage: "/doctor",
        description: "backend + memory-chain readiness",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "add",
        usage: "/add",
        description: "register the launch directory",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "remove",
        usage: "/remove [project]",
        description: "hide a project (history kept)",
        arg: ArgKind::Project,
    },
    CatalogEntry {
        verb: "relabel",
        usage: "/relabel [project] <label>",
        description: "rename a project's label",
        arg: ArgKind::Project,
    },
    CatalogEntry {
        verb: "focus",
        usage: "/focus <panel>",
        description: "move focus to a panel",
        arg: ArgKind::Panel,
    },
    CatalogEntry {
        verb: "profile",
        usage: "/profile <new|rename|switch|delete|duplicate|startup> [name]",
        description: "manage profiles (the menu's grammar)",
        arg: ArgKind::ProfileSub,
    },
    CatalogEntry {
        verb: "quit",
        usage: "/quit",
        description: "leave the dashboard (also /q)",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "checkpoint",
        usage: "/checkpoint [list|show <id>]",
        description: "inspect checkpoints",
        arg: ArgKind::CheckpointSub,
    },
    CatalogEntry {
        verb: "session",
        usage: "/session [list|show <id>]",
        description: "inspect sessions",
        arg: ArgKind::SessionSub,
    },
    CatalogEntry {
        verb: "explain",
        usage: "/explain <checkpoint>",
        description: "roots, chain position, attribution, cost",
        arg: ArgKind::Id,
    },
    CatalogEntry {
        verb: "verify",
        usage: "/verify",
        description: "re-derive + cross-check the hash chain",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "gc",
        usage: "/gc [--dry-run]",
        description: "prune transient refs (never the chain)",
        arg: ArgKind::GcFlag,
    },
    CatalogEntry {
        verb: "projects",
        usage: "/projects [list|prune [--dry-run]]",
        description: "the registry: list rows, sweep dead paths",
        arg: ArgKind::ProjectsSub,
    },
    CatalogEntry {
        verb: "worktrees",
        usage: "/worktrees",
        description: "the family's members: branch, provisioned, prunable",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "adopt",
        usage: "/adopt",
        description: "wire this worktree for the family's enabled agents",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "init",
        usage: "/init",
        description: "scaffold .rift/, hooks, projection (alias /setup)",
        arg: ArgKind::None,
    },
    CatalogEntry {
        verb: "enable",
        usage: "/enable <agent>",
        description: "wire an agent's hooks",
        arg: ArgKind::Agent,
    },
    CatalogEntry {
        verb: "disable",
        usage: "/disable <agent>",
        description: "remove an agent's hook wiring",
        arg: ArgKind::Agent,
    },
    CatalogEntry {
        verb: "configure",
        usage: "/configure [splash <mode>]",
        description: "policy + when the splash plays",
        arg: ArgKind::ConfigureSub,
    },
    CatalogEntry {
        verb: "rebuild",
        usage: "/rebuild",
        description: "rebuild the projection from checkpoints",
        arg: ArgKind::None,
    },
];

/// The in-memory palette frecency (the spec's default: no persistence). An
/// integer use-count per verb word, bumped ONLY on successful dispatch;
/// ranking ties break by count (higher first), then lexicographically.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frecency {
    counts: std::collections::BTreeMap<&'static str, u32>,
}

impl Frecency {
    /// A fresh, empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one successful dispatch of `verb_word` (unknown words no-op —
    /// the log tracks grammar verbs only).
    pub fn bump(&mut self, verb_word: &str) {
        if let Some(entry) = CATALOG.iter().find(|e| e.verb == verb_word) {
            *self.counts.entry(entry.verb).or_insert(0) += 1;
        }
    }

    /// The use count for a verb.
    #[must_use]
    pub fn count(&self, verb_word: &str) -> u32 {
        self.counts.get(verb_word).copied().unwrap_or(0)
    }
}

/// One ranked suggestion row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The text Tab inserts (verb, or verb + argument).
    pub insert: String,
    /// The usage column.
    pub usage: &'static str,
    /// The muted one-line description.
    pub description: &'static str,
}

/// How a candidate matched (ranking bands: exact-prefix over subsequence over
/// description-word; the closed slash grammar's ranking as amended).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Band {
    Prefix,
    Subsequence,
    Description,
}

/// Whether `needle` is a subsequence of `hay` (both lowercase).
fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.by_ref().any(|h| h == c))
}

/// The completion context the reducer supplies (registered project names — the
/// suggest layer never reads state itself).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SuggestContext {
    /// Registered project labels (column-1 order).
    pub projects: Vec<String>,
}

/// Rank suggestions for `buffer` (without its leading `/`). Deterministic for
/// a given buffer, context, and usage log:
/// - empty buffer → the full palette in catalog (common-first) order;
/// - a partial first token → verbs by band (exact-prefix ▸ subsequence ▸
///   description-word), ties by frecency (desc) then verb (lex);
/// - a complete verb + space → context-sensitive ARGUMENT completion per the
///   catalog's [`ArgKind`].
#[must_use]
pub fn suggest(buffer: &str, ctx: &SuggestContext, usage: &Frecency) -> Vec<Suggestion> {
    let body = buffer.strip_prefix('/').unwrap_or(buffer);
    // Argument position: a completed first token followed by whitespace.
    if let Some((verb_word, rest)) = body.split_once(char::is_whitespace) {
        return suggest_args(verb_word, rest.trim_start(), ctx);
    }
    let needle = body.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return CATALOG
            .iter()
            .map(|e| Suggestion {
                insert: e.verb.to_owned(),
                usage: e.usage,
                description: e.description,
            })
            .collect();
    }
    let mut scored: Vec<(Band, std::cmp::Reverse<u32>, &'static str, usize)> = CATALOG
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let band = if e.verb.starts_with(&needle) {
                Band::Prefix
            } else if is_subsequence(&needle, e.verb) {
                Band::Subsequence
            } else if e
                .description
                .to_ascii_lowercase()
                .split_whitespace()
                .any(|w| w.starts_with(&needle))
            {
                Band::Description
            } else {
                return None;
            };
            Some((band, std::cmp::Reverse(usage.count(e.verb)), e.verb, i))
        })
        .collect();
    scored.sort_unstable();
    scored
        .into_iter()
        .filter_map(|(_, _, _, i)| CATALOG.get(i))
        .map(|e| Suggestion {
            insert: e.verb.to_owned(),
            usage: e.usage,
            description: e.description,
        })
        .collect()
}

/// Context-sensitive argument completion (the designed table): registered projects
/// after select/remove/relabel; panels after focus; subcommands after
/// memory/checkpoint/session/projects; splash
/// modes after configure; `--dry-run` after gc.
fn suggest_args(verb_word: &str, partial: &str, ctx: &SuggestContext) -> Vec<Suggestion> {
    let Some(entry) = CATALOG
        .iter()
        .find(|e| e.verb == verb_word.to_ascii_lowercase())
    else {
        return Vec::new();
    };
    let candidates: Vec<String> = match entry.arg {
        ArgKind::None | ArgKind::Id => Vec::new(),
        ArgKind::Project => ctx.projects.clone(),
        ArgKind::Panel => [
            "projects",
            "detail",
            "graphs",
            "checkpoints",
            "timeline",
            "command",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        ArgKind::MemorySub => [
            "diff-since",
            "file-if-changed",
            "cost-hotspots",
            "reread-report",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        ArgKind::CheckpointSub | ArgKind::SessionSub => {
            ["list", "show"].iter().map(|s| (*s).to_owned()).collect()
        }
        ArgKind::ProjectsSub => {
            // The second argument after `prune` is its flag.
            if partial.starts_with("prune") {
                vec!["prune --dry-run".to_owned()]
            } else {
                ["list", "prune"].iter().map(|s| (*s).to_owned()).collect()
            }
        }
        ArgKind::ProfileSub => ["new", "rename", "switch", "delete", "duplicate", "startup"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        ArgKind::ConfigureSub => {
            if let Some(mode_part) = partial.strip_prefix("splash") {
                let mode_needle = mode_part.trim_start().to_ascii_lowercase();
                return ["always", "first-run", "never"]
                    .iter()
                    .filter(|m| m.starts_with(&mode_needle))
                    .map(|m| Suggestion {
                        insert: format!("{verb_word} splash {m}"),
                        usage: entry.usage,
                        description: entry.description,
                    })
                    .collect();
            }
            vec!["splash".to_owned()]
        }
        ArgKind::GcFlag => vec!["--dry-run".to_owned()],
        ArgKind::Agent => vec!["claude-code".to_owned()],
    };
    let needle = partial.to_ascii_lowercase();
    candidates
        .into_iter()
        // A candidate that cannot be TYPED is not a completion. Project names
        // come from the registry, i.e. from directory names, and a control
        // character in one used to be copied verbatim into `insert` — which
        // `input::insert_suggestion` clones into the command buffer, whose
        // grammar cannot spell one. Dropped rather than sanitized on purpose:
        // a stripped name is a DIFFERENT name, so completing it would offer a
        // command that selects nothing, or something else. Such a project is
        // still reachable by index (`/select 3`).
        .filter(|c| !c.chars().any(char::is_control))
        .filter(|c| c.to_ascii_lowercase().starts_with(&needle))
        .map(|c| Suggestion {
            insert: format!("{verb_word} {c}"),
            usage: entry.usage,
            description: entry.description,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::VERB_WORDS;
    use proptest::prelude::*;

    /// The catalog and the grammar cannot drift: exactly one entry per verb
    /// word, no orphans either way, and every entry's verb parses.
    #[test]
    fn catalog_is_in_sync_with_the_grammar() {
        assert_eq!(CATALOG.len(), VERB_WORDS.len(), "one entry per verb");
        for word in VERB_WORDS {
            assert_eq!(
                CATALOG.iter().filter(|e| e.verb == *word).count(),
                1,
                "exactly one catalog entry for `{word}`"
            );
        }
        for entry in CATALOG {
            assert!(
                VERB_WORDS.contains(&entry.verb),
                "catalog orphan `{}`",
                entry.verb
            );
        }
    }

    #[test]
    fn empty_slash_shows_the_full_palette_common_first() {
        let s = suggest("", &SuggestContext::default(), &Frecency::new());
        assert_eq!(s.len(), CATALOG.len());
        assert_eq!(s.first().map(|r| r.insert.as_str()), Some("help"));
        assert_eq!(s.get(1).map(|r| r.insert.as_str()), Some("status"));
    }

    #[test]
    fn exact_prefix_outranks_subsequence_and_description() {
        let s = suggest("se", &SuggestContext::default(), &Frecency::new());
        // Prefix matches (select, session) come before subsequence matches.
        let verbs: Vec<&str> = s.iter().map(|r| r.insert.as_str()).collect();
        let select_at = verbs.iter().position(|v| *v == "select").unwrap();
        let session_at = verbs.iter().position(|v| *v == "session").unwrap();
        for (i, v) in verbs.iter().enumerate() {
            if !v.starts_with("se") {
                assert!(
                    i > select_at && i > session_at,
                    "prefix matches first: {verbs:?}"
                );
            }
        }
    }

    #[test]
    fn frecency_breaks_ties_and_bumps_only_known_verbs() {
        let mut f = Frecency::new();
        // select/session tie on the prefix band; session wins after use.
        f.bump("session");
        let s = suggest("se", &SuggestContext::default(), &f);
        assert_eq!(s.first().map(|r| r.insert.as_str()), Some("session"));
        f.bump("not-a-verb");
        assert_eq!(f.count("not-a-verb"), 0, "unknown words never enter");
    }

    #[test]
    fn argument_completion_follows_the_context_table() {
        let ctx = SuggestContext {
            projects: vec!["rift-cli".into(), "other".into()],
        };
        let f = Frecency::new();
        let insert = |buf: &str| -> Vec<String> {
            suggest(buf, &ctx, &f)
                .into_iter()
                .map(|s| s.insert)
                .collect()
        };
        assert_eq!(insert("select r"), vec!["select rift-cli"]);
        assert_eq!(insert("remove "), vec!["remove rift-cli", "remove other"]);
        assert_eq!(
            insert("focus g"),
            vec!["focus graphs"],
            "panels after focus"
        );
        assert_eq!(
            insert("memory c"),
            vec!["memory cost-hotspots"],
            "subcommands after memory"
        );
        assert_eq!(insert("checkpoint s"), vec!["checkpoint show"]);
        assert_eq!(insert("projects "), vec!["projects list", "projects prune"]);
        assert_eq!(
            insert("configure splash f"),
            vec!["configure splash first-run"],
            "splash modes after configure"
        );
        assert_eq!(insert("gc -"), vec!["gc --dry-run"]);
        assert_eq!(
            insert("explain "),
            Vec::<String>::new(),
            "ids are free-form"
        );
    }

    proptest! {
        /// Suggestions are deterministic for a given buffer, context, and
        /// usage log — and every row's insert is non-empty.
        #[test]
        fn suggestions_are_deterministic(buf in "(?s).{0,40}", bumps in proptest::collection::vec(0..23usize, 0..8)) {
            let context = || SuggestContext {
                projects: vec!["alpha".into(), "beta".into()],
            };
            let log = |order: &dyn Fn(&mut Vec<usize>)| {
                let mut b = bumps.clone();
                order(&mut b);
                let mut f = Frecency::new();
                for i in b {
                    if let Some(e) = CATALOG.get(i) {
                        f.bump(e.verb);
                    }
                }
                f
            };
            // Two calls on one log is a claim about a pure function and nothing
            // more. The claim worth making is that the ranking is a function of
            // the COUNTS and the buffer — not of the order the uses arrived in,
            // and not of which allocation the context strings live in. Reverse
            // the history and rebuild the context; the same multiset of uses is
            // the same log, so the same rows must come back.
            let a = suggest(&buf, &context(), &log(&|_| {}));
            let b = suggest(&buf, &context(), &log(&|v| v.reverse()));
            prop_assert_eq!(&a, &b);
            for row in &a {
                prop_assert!(!row.insert.is_empty());
            }
            // …and one completion is offered once. A palette that listed the
            // same insert twice would spend a row saying nothing.
            let mut inserts: Vec<&str> = a.iter().map(|r| r.insert.as_str()).collect();
            let before = inserts.len();
            inserts.sort_unstable();
            inserts.dedup();
            prop_assert_eq!(inserts.len(), before, "a completion was offered twice");
        }

        /// The same claim, on a buffer where the log actually DECIDES something.
        ///
        /// Measured, not assumed: with an arbitrary 40-character buffer the
        /// property above never reaches a case where frecency breaks a tie — a
        /// random draw essentially never spells a verb prefix — so an
        /// order-dependent log passes it. (Confirmed against a restored defect:
        /// a `Frecency` backed by an arrival-ordered `Vec` is invisible up
        /// there and red down here.) Reach is not assembly; this one is shaped
        /// to a real prefix, which is where the ranking has a choice to make.
        #[test]
        fn ranking_uses_the_counts_and_not_the_order_they_arrived_in(
            idx in 0..CATALOG.len(),
            take in 1usize..3,
            bumps in proptest::collection::vec(0..23usize, 1..8),
        ) {
            let buf: String = CATALOG[idx].verb.chars().take(take).collect();
            let ctx = SuggestContext {
                projects: vec!["alpha".into(), "beta".into()],
            };
            let log = |reversed: bool| {
                let mut b = bumps.clone();
                if reversed {
                    b.reverse();
                }
                let mut f = Frecency::new();
                for i in b {
                    if let Some(e) = CATALOG.get(i) {
                        f.bump(e.verb);
                    }
                }
                f
            };
            prop_assert_eq!(
                suggest(&buf, &ctx, &log(false)),
                suggest(&buf, &ctx, &log(true))
            );
        }
    }
}

#[cfg(test)]
mod untypeable_tests {
    use super::*;

    /// The finding the strengthened property produced, pinned by example: a
    /// registry row whose name carries a control character (directory names can)
    /// is not offered as a completion, while its typeable neighbour still is.
    #[test]
    fn a_project_whose_name_cannot_be_typed_is_not_offered() {
        let ctx = SuggestContext {
            projects: vec!["alpha".into(), "two\nlines".into(), "nul\0".into()],
        };
        let rows = suggest("select ", &ctx, &Frecency::new());
        assert_eq!(
            rows.iter().map(|s| s.insert.clone()).collect::<Vec<_>>(),
            vec!["select alpha".to_owned()],
            "only the typeable candidate survives"
        );
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// A crafted project label can never repaint the terminal from the
        /// panel: whatever bytes a label carries, every rendered row string
        /// passes the shared sanitizer (asserted at the draw seam by
        /// re-sanitizing — sanitize is idempotent on clean text, so a change
        /// here means a control character survived).
        #[test]
        fn suggestion_rows_survive_arbitrary_labels(label in "(?s).{0,40}") {
            let ctx = SuggestContext {
                projects: vec![label],
            };
            for sug in suggest("select ", &ctx, &Frecency::new()) {
                // Asserted on the row AS PRODUCED. The previous assertion
                // sanitized it inside the test and then checked the result,
                // which exercised `sanitize_for_terminal` and said nothing
                // about `suggest` — and the row is not only drawn (where the
                // renderer does sanitize), it is CLONED VERBATIM into the
                // command buffer by `input::insert_suggestion`, whose grammar
                // has no way to spell a control character.
                prop_assert!(
                    !sug.insert.chars().any(char::is_control),
                    "an untypeable row reached the command buffer: {:?}",
                    sug.insert
                );
            }
        }
    }
}
