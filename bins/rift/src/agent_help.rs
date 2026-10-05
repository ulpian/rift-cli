//! `rift agent-help` — the agent's map of Rift's serving plane,
//! rendered **live from the clap command tree**.
//!
//! Nothing here is a hand-maintained copy of the CLI. The verb list is walked
//! from the dispatcher's own [`TOOL_SURFACE`], and each verb's prose, flags,
//! value names, and required-ness are read off the `clap` definitions the
//! binary actually parses with. A verb present in one place and absent from
//! the other is a hard error here rather than a line of guidance teaching a
//! command that does not run — the same "never advertise a dead key"
//! discipline the Canopy modal surfaces hold themselves to.
//!
//! This is also the text the managed skill files point at and the session
//! pointer condenses, so one edit to the CLI updates every agent-facing
//! surface at once.

use clap::CommandFactory as _;
use rift_recall_serve::TOOL_SURFACE;
use serde_json::json;

use crate::cli::{exit, Cli, Outcome};

/// What an agent needs to know before the verb list. Deliberately short: this
/// text is *spend* charged against the savings it enables, so every
/// line has to earn its tokens.
const PREAMBLE: &str = "\
rift — provenance, recall, and cost for AI coding agents.

This repository is captured by Rift: every agent read, edit, and checkpoint
lands in an append-only ledger. Before re-reading files, ask Rift what actually
changed since the state you already hold.

Every verb below is one short-lived process, takes --json, and answers in one
shot. Nothing is interactive and nothing blocks you.

Answers use four states: known_unchanged (you already hold these bytes),
summary_included, exact_included, and available_by_handle (ask for it by
handle, with a reason). Responses are budgeted: content that does not fit is
omitted AND said to be omitted, never silently truncated. Exact retrieval is
policy-gated, requires a reason, and every serve is audited.

Start a turn with known-state, and pass the content hashes you already hold via
--known-hash so Rift can tell you what to skip.";

/// One argument as the agent should see it: its spelling and its help line.
struct ArgLine {
    /// `--task <TEXT>` or `<PATH>`.
    spelling: String,
    /// The clap help text, verbatim — this module reports the CLI's own words
    /// rather than paraphrasing them, so the two can never disagree.
    help: String,
    /// Whether the verb refuses to run without it (drives the usage line).
    required: bool,
    /// Whether the flag accumulates across repeats (a fact for the JSON map;
    /// the help text already says so in prose where it matters).
    repeatable: bool,
}

/// Read one clap argument. Returns `None` for the machinery an agent must not
/// be taught (`--help`/`--version`) and for the globals, which are documented
/// once in the preamble rather than repeated under every verb.
fn arg_line(arg: &clap::Arg) -> Option<ArgLine> {
    let id = arg.get_id().as_str();
    if arg.is_hide_set() || arg.is_global_set() || matches!(id, "help" | "version") {
        return None;
    }
    let value = arg
        .get_value_names()
        .and_then(<[clap::builder::Str]>::first)
        .map_or_else(|| id.to_uppercase(), |n| n.as_str().to_owned());
    // A positional has no long spelling; that test works before clap's build
    // pass assigns positional indices, which is why it is used here. A flag
    // that takes no value (`SetTrue`) is spelled bare — `--full <FULL>` would
    // teach an agent an argument the parser refuses.
    let takes_value = !matches!(
        arg.get_action(),
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse | clap::ArgAction::Count
    );
    let spelling = arg.get_long().map_or_else(
        || format!("<{value}>"),
        |long| {
            if takes_value {
                format!("--{long} <{value}>")
            } else {
                format!("--{long}")
            }
        },
    );
    let help = arg
        .get_help()
        .map_or_else(String::new, |h| h.to_string().replace('\n', " "));
    Some(ArgLine {
        spelling,
        help,
        required: arg.is_required_set(),
        repeatable: matches!(arg.get_action(), clap::ArgAction::Append),
    })
}

/// One verb as the agent should see it.
struct Verb {
    /// The command path as an agent spells it, minus the leading `rift`
    /// (`known-state`, `checkpoint explain`). Owned because it is joined from
    /// the walked path rather than picked out of a hand-written match.
    name: String,
    about: String,
    usage: String,
    args: Vec<ArgLine>,
}

/// Walk the live clap tree for the skill verbs, in the dispatcher's own order.
///
/// # Errors
/// A description of the drift when a dispatcher verb has no CLI subcommand (or
/// the `recall` subtree itself is missing) — the surface refuses to describe
/// itself rather than describe itself wrongly.
fn verbs() -> Result<Vec<Verb>, String> {
    let root = Cli::command();
    let recall = root
        .get_subcommands()
        .find(|c| c.get_name() == "recall")
        .ok_or_else(|| "the `recall` subcommand is absent from the CLI tree".to_owned())?;
    let mut out = Vec::with_capacity(TOOL_SURFACE.len());
    for tool in TOOL_SURFACE.iter().copied() {
        let name = tool.verb_name();
        let sub = recall
            .get_subcommands()
            .find(|c| c.get_name() == name)
            .ok_or_else(|| format!("dispatcher verb `{name}` has no `rift recall` subcommand"))?;
        let args: Vec<ArgLine> = sub.get_arguments().filter_map(arg_line).collect();
        // The same rule as every other taught surface. Byte-identical output
        // here, asserted by the locked snapshot rather than claimed — a third
        // builder is a third place for the rules to drift apart, which is what
        // happened between this one and the map's.
        let usage = taught_usage(&format!("recall {name}"), &args, &[])
            .ok_or_else(|| format!("recall verb `{name}` lost a declared flag"))?;
        out.push(Verb {
            name: name.to_owned(),
            about: sub
                .get_about()
                .map_or_else(String::new, |a| a.to_string().replace('\n', " ")),
            usage,
            args,
        });
    }
    Ok(out)
}

/// The provenance verbs the map teaches beside the recall surface: each is a
/// path into the live clap tree, and absence is
/// a hard error — the same "never advertise a dead key" discipline. None of
/// the three is a different command without a flag, so none declares one.
const PROVENANCE_VERBS: &[TaughtVerb] = &[
    TaughtVerb {
        path: &["blame"],
        flags: &[],
    },
    TaughtVerb {
        path: &["why"],
        flags: &[],
    },
    TaughtVerb {
        path: &["checkpoint", "explain"],
        flags: &[],
    },
];

/// One verb the agent-facing surfaces teach, and the flags it means nothing
/// without.
struct TaughtVerb {
    /// The command path, minus the leading `rift`.
    path: &'static [&'static str],
    /// Long flag names, undashed, whose spellings the skill body must carry.
    /// `rift dispatch` on its own is the HUMAN report (`render_text` /
    /// `render_md`); the agent-facing pack is `--for-agent`. Teaching the bare
    /// verb would teach the wrong command as surely as naming a dead one, so
    /// the flag is declared here by name and its spelling is read off `clap`.
    flags: &'static [&'static str],
}

/// The workflow verbs the map teaches: task-shaped entry points that compile
/// an evidence pack for the agent to reason over. Same discipline as
/// [`PROVENANCE_VERBS`] — a path into the live clap tree, and absence is a
/// hard error rather than a line teaching a dead command.
///
/// `review` and `investigate` are the two investigation verbs. `dispatch
/// --for-agent` and `recap --json` are here because the agent-facing set was
/// settled to include them: a census row marking them untaught would have
/// recorded a divergence from a binding decision *as a decision*, which is a
/// false sentence wearing a data structure.
const WORKFLOW_VERBS: &[TaughtVerb] = &[
    TaughtVerb {
        path: &["review"],
        flags: &[],
    },
    TaughtVerb {
        path: &["investigate"],
        flags: &[],
    },
    TaughtVerb {
        path: &["dispatch"],
        flags: &["for-agent"],
    },
    TaughtVerb {
        path: &["recap"],
        flags: &[],
    },
];

/// The spelling a taught verb is advertised with — **the one rule, for every
/// surface that advertises one**: the command path, then every positional,
/// then every argument clap marks required, then any flag the verb is a
/// different command without. `[options]` closes the line for whatever is
/// left, when the surface lists those flags underneath.
///
/// EVERY POSITIONAL, not only the required ones, and `investigate` is why:
/// `seed` is `Option<String>`, so clap calls it optional while
/// `rift investigate` with no seed exits USAGE. Clap's optionality says
/// whether the parser accepts the argument's absence, not whether the verb
/// works without it — and `[options]` cannot supply a positional anyway.
///
/// EVERY REQUIRED ARGUMENT TOO, and that clause is here because I left it out
/// and the dump caught it: without it `rift recall known-state --task <TEXT>
/// --file <PATH> [options] --json` collapsed to `rift recall known-state
/// [options] --json`, and `retrieve-exact` lost the mandatory, audited
/// `--reason <TEXT>`. Unifying two rules is not an excuse to drop what the
/// other one knew.
///
/// This was two rules for one commit, and the second shipped the exact defect
/// the first was written to fix: the map built usage from *required* args
/// only, so it taught `rift investigate [options] --json` (exits USAGE) and
/// `rift dispatch [options] --json` (the human `render_text`/`render_md` arm,
/// not the agent's pack) — the two spellings
/// `the_skill_body_teaches_every_workflow_verb_from_the_tree` names as wrong,
/// in a surface that is enumerated as agent-facing.
///
/// `None` when a declared flag is not on the verb any more: a renamed
/// `--for-agent` must drop the line rather than teach a spelling the parser
/// refuses, and the suite reds on the missing line.
fn taught_usage(path: &str, args: &[ArgLine], flags: &[&str]) -> Option<String> {
    let positional = |a: &ArgLine| a.spelling.starts_with('<');
    let is_flag = |a: &ArgLine, flag: &str| {
        let long = format!("--{flag}");
        a.spelling == long || a.spelling.starts_with(&format!("{long} "))
    };
    let declared = |a: &ArgLine| flags.iter().any(|f| is_flag(a, f));

    let mut shown: Vec<&str> = Vec::new();
    for a in args.iter().filter(|a| positional(a)) {
        shown.push(&a.spelling);
    }
    for a in args.iter().filter(|a| a.required && !positional(a)) {
        shown.push(&a.spelling);
    }
    for flag in flags {
        let spelling = args.iter().find(|a| is_flag(a, flag))?.spelling.as_str();
        if !shown.contains(&spelling) {
            shown.push(spelling);
        }
    }

    let mut out = format!("rift {path}");
    for spelling in &shown {
        out.push(' ');
        out.push_str(spelling);
    }
    let unnamed = args
        .iter()
        .any(|a| !positional(a) && !a.required && !declared(a));
    if unnamed {
        out.push_str(" [options]");
    }
    out.push_str(" --json");
    Some(out)
}

/// Walk the live clap tree for one taught verb table. `kind` names the table
/// in the drift message, so a failure says which list went stale and not only
/// which verb vanished.
///
/// # Errors
/// A description of the drift when a taught path has no subcommand, or when a
/// verb has lost a flag its table declares.
fn walk_verbs(taught: &[TaughtVerb], kind: &str) -> Result<Vec<Verb>, String> {
    let root = Cli::command();
    let mut out = Vec::with_capacity(taught.len());
    for t in taught {
        let mut node = &root;
        for seg in t.path {
            node = node
                .get_subcommands()
                .find(|c| c.get_name() == *seg)
                .ok_or_else(|| {
                    format!(
                        "taught {kind} verb `{}` has no subcommand",
                        t.path.join(" ")
                    )
                })?;
        }
        let args: Vec<ArgLine> = node.get_arguments().filter_map(arg_line).collect();
        let path = t.path.join(" ");
        let usage = taught_usage(&path, &args, t.flags).ok_or_else(|| {
            format!(
                "taught {kind} verb `{path}` has lost a flag its table declares: {:?}",
                t.flags
            )
        })?;
        out.push(Verb {
            name: path,
            about: node
                .get_about()
                .map_or_else(String::new, |a| a.to_string().replace('\n', " ")),
            usage,
            args,
        });
    }
    Ok(out)
}

/// Walk the live clap tree for the provenance verbs.
///
/// # Errors
/// See [`walk_verbs`].
fn provenance_verbs() -> Result<Vec<Verb>, String> {
    walk_verbs(PROVENANCE_VERBS, "provenance")
}

/// Walk the live clap tree for the workflow verbs, in table order.
///
/// # Errors
/// See [`walk_verbs`].
fn workflow_verbs() -> Result<Vec<Verb>, String> {
    walk_verbs(WORKFLOW_VERBS, "workflow")
}

/// Append one headed section of verbs, each with its usage line, the CLI's own
/// about text, and its arguments.
fn push_section(out: &mut String, heading: &str, verbs: &[Verb]) {
    out.push_str(heading);
    for v in verbs {
        out.push_str(&format!("\n  {}\n      {}\n", v.usage, v.about));
        for a in &v.args {
            out.push_str(&format!("      {:<24} {}\n", a.spelling, a.help));
        }
    }
}

/// Render the agent-facing help text.
///
/// # Errors
/// See [`verbs`].
pub(crate) fn render() -> Result<String, String> {
    let mut out = String::from(PREAMBLE);
    push_section(&mut out, "\n\nVerbs:\n", &verbs()?);
    push_section(
        &mut out,
        "\nProvenance verbs (who wrote it, under which prompt, at what cost):\n",
        &provenance_verbs()?,
    );
    push_section(
        &mut out,
        "\nWorkflow verbs (Rift compiles the evidence; the reasoning is yours):\n",
        &workflow_verbs()?,
    );
    out.push_str(
        "\nThis text is generated from the installed binary's own command tree,\n\
         so it always describes the version you are running.\n",
    );
    Ok(out)
}

/// The thin pointer body written into each family's managed skill file.
/// Deliberately a handful of lines: its job is to name
/// `rift agent-help`, which renders the full map from the live command tree.
/// Copying the map here would create a second source of truth that goes stale
/// the moment the CLI moves.
///
/// The document line is here for the same reason the rest is: the hook is what
/// actually points a document read at the Markdown beside it, and this file is
/// the fallback for the session where the hook said nothing. It names the
/// directory rather than a conversion, because the directory is the one fact
/// an agent can act on without asking anybody anything.
#[must_use]
pub(crate) fn skill_body() -> String {
    "Rift automatically captures this repository and delivers bounded recall context through native hooks.\n\
     - `rift agent-help` — inspect the live recall and recovery surface.\n\
     - `rift recall status --json` — verify hook, runtime, context, and degradation status.\n\
     Documents (.docx, .pdf, .xlsx and the like) are converted once for this repository and kept as Markdown under `.rift/state/doc/`; a hook points a document read there, or names the file to read in its place. Read that Markdown rather than the binary, or ask `rift recall doc-read <path>` for it; where there is none, read the document as usual.\n\
     If Rift reports degradation, let the raw agent call run and use the diagnostic above; correctness never depends on this file."
        .to_owned()
}

/// The once-per-session pointer injected through a family's context channel.
/// Around a hundred tokens: this is spend the agent did not
/// ask for, so it says only that the plane exists, names the one command that
/// explains it, and shows the single call worth making first.
///
/// Assembled from the dispatcher's verb spellings for the same reason the
/// skill body is: an injected pointer that names a dead command would be worse
/// than no pointer at all.
#[must_use]
pub(crate) fn session_pointer() -> String {
    use rift_recall_serve::ToolName as T;
    format!(
        "Rift is capturing this repository and knows what changed since the state you\n\
         already hold. Ask it before re-reading files:\n\
         \x20 rift agent-help                 the map of the recall surface\n\
         \x20 rift recall {ks} --task <goal> --file <path> --json\n\
         \x20 rift recall {ds} --known-hash <sha256> --json\n\
         \x20 rift why <path>:<line> --json    who wrote a line, its prompt and cost\n\
         Every delta carries that provenance already: when, who, what it cost.\n\
         Answers are budgeted and audited, and say what they omitted.",
        ks = T::KnownState.verb_name(),
        ds = T::DiffSince.verb_name(),
    )
}

/// The command a per-turn delta manifest names for detail: the pull for the
/// first file it names, spelled with that file's own path and the revision the
/// session holds.
///
/// Assembled from the dispatcher's verb spelling for the same reason the
/// pointer is, and deliberately free of placeholder brackets: the push-side
/// chokepoint's charset allowlist refuses `<` and `>`, so a hint spelled
/// `--known-hash <sha256>` is withheld and takes the manifest's whole "detail
/// on demand" line with it. That is why the hint carried no hash at all until
/// an entry carried one: a command with a placeholder in it is not a command,
/// and the only alternative to a placeholder is a real value.
///
/// Sixteen hex is the argument form the pull verbs resolve a basis from
/// (twelve is the display form, and nothing shorter is a hash at all), so a
/// caller whose revision is anything else — no hash, a truncated one, a span
/// that is not hex — is answered with the verb that needs no basis. So is a
/// path with a space in it: this module owns no quoting rule, and a command
/// that parses as two arguments is worse than a more general one that runs.
#[must_use]
pub(crate) fn manifest_pull_hint(path: &str, hash: &str) -> String {
    use rift_recall_serve::ToolName as T;
    let basis = (hash.len() >= 16 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| {
        hash.chars()
            .take(16)
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    });
    match basis {
        Some(basis) if !path.is_empty() && !path.contains(' ') => format!(
            "rift recall {} {path} --known-hash {basis} --json",
            T::ChangedHunks.verb_name()
        ),
        _ => format!("rift recall {} --json", T::DiffSince.verb_name()),
    }
}

/// Everything `run` renders: the human map, and the three taught tables the
/// `--json` document carries as separate arrays.
///
/// A struct rather than a tuple because the three vectors are the same *shape*
/// and three different *things* — the closed recall surface an agent shells
/// out to, the provenance verbs, and the workflow verbs — and which is which
/// is the most useful thing the map knows. In a tuple that distinction lives
/// only in a position a reader has to count.
struct Described {
    /// The rendered human text; identical to the `text` field of the JSON.
    text: String,
    /// The closed serving surface, in the dispatcher's own order
    /// ([`TOOL_SURFACE`]).
    recall: Vec<Verb>,
    /// Who wrote a line, under which prompt, at what cost.
    provenance: Vec<Verb>,
    /// Task-shaped entry points that compile an evidence pack for the agent to
    /// reason over.
    workflow: Vec<Verb>,
}

/// Describe this build, or report the first drift the tree walk found.
///
/// # Errors
/// See [`verbs`] and [`walk_verbs`].
fn described() -> Result<Described, String> {
    Ok(Described {
        text: render()?,
        recall: verbs()?,
        provenance: provenance_verbs()?,
        workflow: workflow_verbs()?,
    })
}

/// `rift agent-help` — human text, or the same map as data under `--json`.
pub(crate) fn run() -> Outcome {
    let Described {
        text,
        recall,
        provenance,
        workflow,
    } = match described() {
        Ok(parts) => parts,
        Err(why) => {
            return Outcome::coded(
                exit::INTERNAL,
                format!("agent-help cannot describe this build: {why}"),
                json!({ "error": "cli_surface_drift", "reason": why }),
            )
        }
    };
    let doc = json!({
        "preamble": PREAMBLE,
        "verbs": recall.iter().map(|v| json!({
            "verb": v.name,
            "about": v.about,
            "usage": v.usage,
            "args": v.args.iter().map(|a| json!({
                "spelling": a.spelling,
                "help": a.help,
                "required": a.required,
                "repeatable": a.repeatable,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "provenance_verbs": provenance.iter().map(|v| json!({
            "verb": v.name,
            "about": v.about,
            "usage": v.usage,
        })).collect::<Vec<_>>(),
        "workflow_verbs": workflow.iter().map(|v| json!({
            "verb": v.name,
            "about": v.about,
            "usage": v.usage,
        })).collect::<Vec<_>>(),
        "text": text,
    });
    Outcome::ok(text, doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path and a full 64-hex content hash, so every hint test spells the
    /// argument form from the same input the product renders from.
    const SRC: &str = "src/lib.rs";
    const HASH: &str = "3f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c";

    /// The hint is a command, not a template: real path, real basis, no
    /// bracket an agent would have to fill in itself — and the fallback is the
    /// verb that needs no basis rather than a broken spelling of the one that
    /// does.
    ///
    /// The placeholder half is not stylistic. The push guard's charset
    /// allowlist refuses `<` and `>`, so a templated hint is withheld and the
    /// manifest silently loses its whole detail line: an unrunnable hint and
    /// no hint are the same outcome, reached without anyone noticing.
    #[test]
    fn the_hint_is_runnable_and_placeholder_free() {
        let hint = manifest_pull_hint(SRC, HASH);
        assert_eq!(
            hint,
            "rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json"
        );
        // Sixteen hex of the revision the session holds — the argument form,
        // not the twelve-hex display form.
        assert!(hint.contains(&HASH[..16]));
        assert!(!hint.contains(&HASH[..17]), "the whole hash is 40 tokens");
        // Every basis a caller can hold that is not a basis falls back to the
        // verb that needs none, and the fallback is still a live command.
        for (path, hash) in [
            (SRC, ""),
            (SRC, "3f9a1c2e8b7d"),
            (SRC, "not-hex-at-all-not-hex"),
            ("", HASH),
            ("docs/my report.md", HASH),
        ] {
            assert_eq!(
                manifest_pull_hint(path, hash),
                "rift recall diff-since --json",
                "{path:?} {hash:?}"
            );
        }
        for hint in [manifest_pull_hint(SRC, HASH), manifest_pull_hint("", "")] {
            assert!(!hint.contains('<') && !hint.contains('>'), "{hint}");
            assert!(hint.is_ascii(), "{hint}");
            let named = commands_named(&hint).expect("the hint names a live command");
            assert_eq!(named.len(), 1, "{hint}: {named:?}");
            let verb = named[0]
                .strip_prefix("recall ")
                .expect("the hint is a recall verb");
            assert!(
                rift_recall_serve::ToolName::from_verb(verb).is_some(),
                "{verb} is not a dispatcher verb"
            );
        }
    }

    /// The hint is a command this binary parses — asserted by parsing it,
    /// through the same argv path a shell would take.
    ///
    /// It is the whole point of the line and it was not true when the hint was
    /// first written: the verb wanted `--task <TEXT> --file <PATH>` and the
    /// hinted form exited with a usage error, so every manifest and every
    /// decline frame carried a command that could not run. A test that only
    /// reads the string cannot see that; this one hands it to `clap`.
    #[test]
    fn the_rendered_pull_hint_parses_as_a_command() {
        use clap::Parser as _;
        let hint = manifest_pull_hint(SRC, HASH);
        let argv: Vec<&str> = hint.split(' ').filter(|a| *a != "--json").collect();
        let parsed = crate::cli::Cli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("the hint does not parse: {hint}\n{e}"));
        let Some(crate::cli::Command::Recall(crate::cli::RecallCmd::ChangedHunks(a))) = parsed.cmd
        else {
            panic!("the hint is not the pull verb: {hint}");
        };
        // The path and the basis land where the dispatcher reads them, and the
        // ranking params it does not need are absent rather than empty
        // strings.
        assert_eq!(a.target.as_deref(), Some(SRC));
        assert_eq!(a.known_hashes, vec![HASH[..16].to_owned()]);
        assert!(a.task.is_none());
        assert!(a.files.is_empty());

        // The fallback the renderer emits when a caller holds no usable
        // revision is a command too.
        let fallback = manifest_pull_hint("", "");
        let argv: Vec<&str> = fallback.split(' ').filter(|a| *a != "--json").collect();
        assert!(
            crate::cli::Cli::try_parse_from(&argv).is_ok(),
            "the fallback does not parse: {fallback}"
        );

        // The DECLINE frames end with the same line, written by the sealed
        // renderer rather than by this module, and a frame that names a
        // command nobody can run is the same defect wherever it is rendered.
        // So the frame's own text is parsed here too — the sentence a model
        // reads is the thing under test, not a spelling this file controls.
        // A frame now ends with TWO commands — the pull, and the provenance
        // handle that answers why the file moved — so both are parsed. A
        // handle an agent cannot run is exactly the defect this test exists
        // for, and it would otherwise hide behind the pull in front of it.
        let frame = rift_recall_serve::render_deny_frame(SRC, HASH, 1, 40, Some(3));
        let (pull, why) = frame
            .rsplit_once("rift recall ")
            .map(|(_, rest)| format!("rift recall {rest}"))
            .and_then(|tail| {
                tail.split_once(" Provenance: ")
                    .map(|(pull, why)| (pull.to_owned(), why.to_owned()))
            })
            .expect("the frame carries a pull and a provenance handle");
        for command in [&pull, &why] {
            let argv: Vec<&str> = command.split(' ').filter(|a| *a != "--json").collect();
            assert!(
                crate::cli::Cli::try_parse_from(&argv).is_ok(),
                "the decline frame names a command that does not parse: {command}"
            );
        }
        assert_eq!(pull, hint, "two spellings of one pull");
        assert_eq!(why, format!("rift why {SRC}:1 --json"));
    }

    /// The anti-drift gate: the map describes exactly the dispatcher's closed
    /// surface, and every verb it teaches is a command this binary parses.
    #[test]
    fn the_map_covers_the_closed_surface_and_teaches_nothing_dead() {
        let verbs = verbs().expect("the CLI tree carries every dispatcher verb");
        assert_eq!(verbs.len(), TOOL_SURFACE.len());
        for (v, tool) in verbs.iter().zip(TOOL_SURFACE.iter().copied()) {
            assert_eq!(v.name, tool.verb_name());
            assert!(!v.about.is_empty(), "{} has no about line", v.name);
            assert!(
                rift_recall_serve::ToolName::from_verb(&v.name).is_some(),
                "{} is not a dispatcher verb",
                v.name
            );
            for a in &v.args {
                assert!(!a.help.is_empty(), "{} {} has no help", v.name, a.spelling);
                // Every positional and every value-taking flag names its
                // value. A switch is spelled bare on purpose — `--content
                // <CONTENT>` would teach an argument the parser refuses — so
                // the claim is stated as the two shapes rather than as one: a
                // bare spelling is a single `--long` token and nothing else,
                // and a positional never reaches that arm.
                assert!(
                    a.spelling.contains('<') || !a.spelling.contains(' '),
                    "{} {} teaches neither a value name nor a bare switch",
                    v.name,
                    a.spelling
                );
                assert!(
                    a.spelling.contains('<') || a.spelling.starts_with("--"),
                    "{} {} is a positional with no value name",
                    v.name,
                    a.spelling
                );
            }
        }
    }

    /// Globals and clap machinery stay out of the per-verb listing: an agent
    /// reading this must not be taught `--help` as if it were recall surface.
    #[test]
    fn globals_and_clap_machinery_are_not_taught_as_verb_arguments() {
        for v in &verbs().unwrap() {
            for a in &v.args {
                for forbidden in ["--help", "--version", "--json", "--quiet", "--color"] {
                    assert!(
                        !a.spelling.starts_with(forbidden),
                        "{} teaches {forbidden}",
                        v.name
                    );
                }
            }
        }
    }

    /// Injected and installed guidance is *spend*: the session pointer is
    /// capped so it can never grow into a manifest by accident (the per-turn
    /// manifest has its own, larger budget; this is the once-per-session
    /// pointer's).
    #[test]
    fn the_session_pointer_stays_within_its_token_budget() {
        const POINTER_CAP: u32 = 150;
        let tokens = rift_tokens::count(&session_pointer());
        assert!(
            tokens <= POINTER_CAP,
            "the session pointer costs {tokens} tokens (cap {POINTER_CAP})"
        );
        assert!(tokens > 0);
    }

    /// The provenance half of the map, locked: the three
    /// verbs, walked from the live tree, with runnable usage lines — and the
    /// `--json` document carries them under `provenance_verbs`.
    #[test]
    fn the_provenance_map_walks_the_live_tree_and_rides_the_json_contract() {
        let verbs = provenance_verbs().expect("provenance verbs resolve");
        let names: Vec<&str> = verbs.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["blame", "why", "checkpoint explain"]);
        assert!(
            verbs[0].usage.starts_with("rift blame <PATH>"),
            "{}",
            verbs[0].usage
        );
        assert!(
            verbs[1].usage.starts_with("rift why <PATH[:LINE]>"),
            "{}",
            verbs[1].usage
        );
        assert!(
            verbs[2].usage.starts_with("rift checkpoint explain"),
            "{}",
            verbs[2].usage
        );
        for v in &verbs {
            assert!(
                v.usage.ends_with("--json"),
                "agent usage is --json: {}",
                v.usage
            );
        }

        let out = run();
        assert_eq!(out.code, exit::OK);
        let taught: Vec<&str> = out.json["provenance_verbs"]
            .as_array()
            .expect("provenance_verbs rides the json contract")
            .iter()
            .filter_map(|v| v["verb"].as_str())
            .collect();
        assert_eq!(taught, ["blame", "why", "checkpoint explain"]);
        assert!(
            out.human.contains("Provenance verbs"),
            "the human map carries the section"
        );
    }

    /// Every `rift …` command a piece of guidance names, walked out of the
    /// live clap tree word by word.
    ///
    /// The walk is what makes the check derive rather than mirror: the first
    /// word after `rift` MUST be a real subcommand, and while the node it
    /// lands on requires a subcommand of its own the next word must be one
    /// too. A word that resolves to nothing and is not required to ends the
    /// command — which is how prose (`— the full map of …`) stops being
    /// mistaken for a verb without a hand-written list of what prose looks
    /// like. Guidance naming something dead comes back as `Err`, so a caller
    /// can assert on the message instead of on a panic.
    fn commands_named(text: &str) -> Result<Vec<String>, String> {
        let root = Cli::command();
        let mut found = Vec::new();
        for line in text.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            let Some(pos) = words
                .iter()
                .position(|w| w.trim_start_matches('`') == "rift")
            else {
                continue;
            };
            let mut node = &root;
            let mut path: Vec<&str> = Vec::new();
            let mut at = pos + 1;
            loop {
                let word = words.get(at).map_or("", |w| w.trim_matches('`'));
                match node.get_subcommands().find(|c| c.get_name() == word) {
                    Some(sub) => {
                        path.push(word);
                        node = sub;
                        at += 1;
                    }
                    None if path.is_empty() => {
                        return Err(format!("guidance names a dead command: `rift {word}`"));
                    }
                    None if node.is_subcommand_required_set() => {
                        return Err(format!(
                            "guidance names a dead verb: `rift {} {word}`",
                            path.join(" ")
                        ));
                    }
                    None => break,
                }
            }
            found.push(path.join(" "));
        }
        Ok(found)
    }

    /// Neither the pointer nor the skill body may name a command this binary
    /// does not have: guidance the agent cannot act on is worse than none.
    ///
    /// Nothing here enumerates the commands guidance is allowed to name — the
    /// tree decides, so teaching a new verb needs no edit to this test, and a
    /// verb that is renamed or deleted reds it without one either.
    #[test]
    fn injected_and_installed_guidance_names_only_live_commands() {
        // The minimum is per surface: the pointer and the skill body are
        // maps and must teach several commands, while the manifest hint is one
        // line inside a budgeted manifest and is meant to be exactly one.
        for (surface, text, least) in [
            ("the session pointer", session_pointer(), 3),
            ("the skill body", skill_body(), 2),
            ("the manifest hint", manifest_pull_hint(SRC, HASH), 1),
            (
                "the basis-less manifest hint",
                manifest_pull_hint("", ""),
                1,
            ),
        ] {
            let named = match commands_named(&text) {
                Ok(named) => named,
                Err(why) => panic!("{surface}: {why}"),
            };
            for command in &named {
                if let Some(verb) = command.strip_prefix("recall ") {
                    assert!(
                        verb == "status" || rift_recall_serve::ToolName::from_verb(verb).is_some(),
                        "{surface} names `{verb}`, which is neither status nor a dispatcher verb"
                    );
                }
            }
            assert!(
                named.len() >= least,
                "guidance that teaches nothing is not guidance: {surface} named {named:?}"
            );
        }
    }

    /// Every top-level `rift` subcommand, in the tree's own order, and whether
    /// agent-facing guidance teaches it — with the reason on the line either
    /// way.
    ///
    /// **What this catches, and what it does not.** It catches a subcommand
    /// added to or removed from the CLI without anyone deciding whether an
    /// agent should be told it exists: the census stops matching the tree and
    /// the failure names the command. It does **not** know that a new command
    /// is agent-facing — nothing in the `clap` tree says so, and the two verbs
    /// this census was written for sat in the tree for weeks with no guidance
    /// naming them and nothing red. So it forces the classification instead of
    /// making it, and a row written `false` with an untrue reason is a lie
    /// this test cannot see. The reason column exists so the lie has to be
    /// typed out.
    const SURFACE_CENSUS: &[(&str, bool, &str)] = &[
        (
            "init",
            false,
            "operator lifecycle: a human wires the repository",
        ),
        (
            "status",
            false,
            "human summary; the agent asks the recall plane",
        ),
        (
            "viewer",
            false,
            "no `--json` arm at all: intercepted before the dispatcher, and off a \
             TTY it prints a rendered dashboard frame",
        ),
        ("doctor", false, "operator diagnostics"),
        ("clean", false, "operator lifecycle: destructive-ish GC"),
        (
            "enable",
            false,
            "operator lifecycle: wires one agent's hooks",
        ),
        ("disable", false, "operator lifecycle"),
        ("verify", false, "operator integrity check"),
        ("rebuild", false, "operator recovery"),
        ("backfill", false, "operator recovery"),
        ("import", false, "operator migration"),
        ("chain", false, "operator integrity surface"),
        (
            "checkpoint",
            true,
            "`checkpoint explain` — this commit's provenance",
        ),
        ("session", false, "operator session control"),
        ("sync", false, "operator transport"),
        ("bundle", false, "operator transport"),
        (
            "dispatch",
            true,
            "`--for-agent` — the window as a budgeted pack (agent-facing)",
        ),
        ("recap", true, "offline usage analytics (agent-facing)"),
        ("review", true, "compiles the review evidence pack"),
        (
            "investigate",
            true,
            "compiles an investigation evidence pack",
        ),
        (
            "explain",
            false,
            "`checkpoint explain` is the taught spelling",
        ),
        ("blame", true, "per-span authorship for a file"),
        ("why", true, "one line's author, prompt and cost"),
        (
            "savings",
            false,
            "the owner's evaluation figure over this repository's own \
             measurement, read by a person and drawn on the dashboard; the \
             serving surface consumes no scorecard, and an agent that asked \
             for one would learn nothing it could act on. Its one subcommand, \
             `savings reset`, is an OWNER act and doubly not agent-facing: it \
             moves the boundary the figure the owner judges this plane by is \
             measured from, so an agent that could call it could reset the \
             scoreboard it is being scored on",
        ),
        (
            "memory",
            false,
            "the human twin of `recall`; the agent uses `recall`",
        ),
        ("recall", true, "the closed serving surface"),
        (
            "doc",
            false,
            "an ALIAS, and the row is false because that is the checkable \
             claim: `doc read` parses into the same verb the map teaches as \
             `recall doc-read`, and guidance names one spelling rather than \
             two. Nothing reaches an agent through this that the taught \
             spelling does not already carry",
        ),
        (
            "search",
            false,
            "an ALIAS, and false for the reason the `doc` row is: `rift search` \
             parses into the same verb the map teaches as `recall search` and \
             routes through the same function, and guidance names one spelling \
             rather than two. The verb itself IS agent-facing — it is on the \
             closed surface and the map teaches it — which is exactly why a \
             second row claiming to teach it would be a duplicate, not a \
             disclosure",
        ),
        ("agent-help", true, "the map every other pointer defers to"),
        ("gc", false, "operator lifecycle"),
        (
            "projects",
            false,
            "machine-global registry, not repository state",
        ),
        (
            "family",
            false,
            "operator repair: its one subcommand rewrites this repository's \
             recorded identity and refuses without `--yes`, so it is a decision \
             a human takes, not a fact an agent reads",
        ),
        (
            "worktree",
            false,
            "operator wiring: `adopt` provisions a linked worktree for the \
             agents the family enabled, which is a machine setup act — the \
             agent running inside that worktree consumes the result, and has \
             nothing to ask about it",
        ),
        ("configure", false, "operator policy"),
        (
            "hooks",
            false,
            "the agent/git wire entry; hidden, never advertised",
        ),
    ];

    /// The census matches the tree. The short fallback skill deliberately
    /// defers this full classification to `rift agent-help`.
    #[test]
    fn every_top_level_command_is_classified() {
        let root = Cli::command();
        let live: Vec<&str> = root
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect();
        let censused: Vec<&str> = SURFACE_CENSUS.iter().map(|(name, ..)| *name).collect();
        assert_eq!(
            live, censused,
            "the CLI moved without a census entry: classify the command as taught, \
             or record why an agent is not told about it"
        );

        for (name, _is_taught, reason) in SURFACE_CENSUS {
            assert!(!reason.is_empty(), "`rift {name}` has no reason recorded");
        }
    }

    /// `savings reset` is the only subcommand under a not-taught row that
    /// *writes*, so the row's reason has to name it — a census that classifies
    /// `rift savings` as a scorecard nobody needs would otherwise silently be
    /// classifying the verb that moves the scoreboard as well.
    ///
    /// Two facts, both checkable: it parses (so it is a real verb somebody has
    /// to have decided about), and no agent-facing guidance names it. The
    /// second is the one that matters — guidance is the only thing that puts a
    /// verb in front of a model.
    #[test]
    fn the_savings_reset_verb_is_an_owner_act_and_no_guidance_teaches_it() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from(["rift", "savings", "reset"]).expect("it parses");
        assert!(matches!(
            cli.cmd,
            Some(crate::cli::Command::Savings(crate::cli::SavingsArgs {
                cmd: Some(crate::cli::SavingsCmd::Reset(_)),
                ..
            }))
        ));
        let row = SURFACE_CENSUS
            .iter()
            .find(|(name, ..)| *name == "savings")
            .expect("savings is censused");
        assert!(!row.1, "the scorecard is not taught");
        assert!(
            row.2.contains("savings reset"),
            "the row has to name the write verb it covers: {}",
            row.2
        );
        let map = run().human;
        assert!(
            !map.contains("savings reset"),
            "the agent map must not teach the verb that moves the window"
        );
    }

    /// `rift viewer` is classified `false` on a ROUTING FACT, not on a sentence
    /// that sounds right — and it is the first row written since I recorded
    /// that a `false` row with an untrue reason passes this census unchallenged.
    ///
    /// The obvious reason ("an agent cannot read a TUI") is not the true one and
    /// would not have been checkable. `viewer` off a TTY does not refuse: it
    /// prints a rendered dashboard frame. The real disqualifier is that it is
    /// intercepted in `main` BEFORE the dispatcher and its `route` arm is an
    /// error, so it can never produce an `Outcome` — and `Outcome` is what
    /// carries the `--json` document every taught verb answers with.
    ///
    /// **The clap tree cannot tell you this.** `--json` is a global, so
    /// `rift viewer --json` parses exactly like `rift recall diff-since --json`,
    /// and every other guard in this file would see a well-formed command. Only
    /// routing separates them, so routing is what this asserts. Give `viewer` a
    /// real dispatcher arm and this reds — which is the point: the row would
    /// then be false and has to be re-decided.
    #[test]
    fn viewer_is_untaught_because_it_cannot_answer_in_json() {
        use clap::Parser as _;
        let here = std::path::Path::new(".");

        let cli = Cli::try_parse_from(["rift", "viewer"]).expect("`rift viewer` parses");
        let cmd = cli.cmd.as_ref().expect("viewer is a subcommand");
        assert!(
            crate::route(&cli, cmd, here).is_err(),
            "`rift viewer` gained a dispatcher arm and may now be able to answer in \
             --json; re-decide its SURFACE_CENSUS row rather than leaving it false"
        );

        // Control, so the assertion above cannot pass by `route` being broken
        // for everything: a TAUGHT command that needs no repository state does
        // route, and does answer.
        let taught = Cli::try_parse_from(["rift", "agent-help"]).expect("parses");
        let taught_cmd = taught.cmd.as_ref().expect("agent-help is a subcommand");
        assert!(
            crate::route(&taught, taught_cmd, here).is_ok(),
            "the control command stopped routing; this test proves nothing now"
        );
    }

    /// The commands the once-per-session pointer names are PINNED rather than
    /// derived, because the pointer's cost is the point: it is spend the agent
    /// did not ask for, injected on every session, and every line in it has to
    /// buy its tokens back.
    ///
    /// `review` and `investigate` are deliberately absent. They are not the
    /// first call of a turn — they are task-shaped, reached when a human asks
    /// for a review or an investigation — and the pointer already names
    /// `rift agent-help`, which since this commit carries them both with their
    /// flags. Adding them here is a budget decision; this test makes it one
    /// instead of letting it happen by edit.
    const POINTER_COMMANDS: &[&str] = &[
        "agent-help",
        "recall known-state",
        "recall diff-since",
        "why",
    ];

    #[test]
    fn the_session_pointer_names_exactly_the_commands_its_budget_bought() {
        let named = commands_named(&session_pointer()).expect("the pointer names live commands");
        assert_eq!(named, POINTER_COMMANDS);
    }

    /// The workflow half of the map: the four task-shaped verbs, walked from
    /// the live tree, with runnable usage lines — and the `--json` document
    /// carries them under `workflow_verbs`.
    #[test]
    fn the_workflow_map_walks_the_live_tree_and_rides_the_json_contract() {
        let verbs = workflow_verbs().expect("workflow verbs resolve");
        let names: Vec<&str> = verbs.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["review", "investigate", "dispatch", "recap"]);
        for v in &verbs {
            assert!(!v.about.is_empty(), "{} has no about line", v.name);
            assert!(
                v.usage.ends_with("--json"),
                "agent usage is --json: {}",
                v.usage
            );
        }

        let out = run();
        assert_eq!(out.code, exit::OK);
        let taught: Vec<&str> = out.json["workflow_verbs"]
            .as_array()
            .expect("workflow_verbs rides the json contract")
            .iter()
            .filter_map(|v| v["verb"].as_str())
            .collect();
        assert_eq!(taught, ["review", "investigate", "dispatch", "recap"]);
        assert!(
            out.human.contains("Workflow verbs"),
            "the human map carries the section"
        );
    }

    /// The installed block is fallback documentation only: it points at the
    /// live map and status, and carries no workflow correctness dependency.
    #[test]
    fn the_skill_body_is_a_short_diagnostic_fallback() {
        let body = skill_body();
        assert!(body.contains("`rift agent-help`"), "{body}");
        assert!(body.contains("`rift recall status --json`"), "{body}");
        assert!(
            body.contains("correctness never depends on this file"),
            "{body}"
        );
        // The document pointer is the one place this file names a directory:
        // where the Markdown for a document lives, and that reading it is a
        // fallback for the session whose hook said nothing — never a step the
        // agent is required to take.
        assert!(body.contains("`.rift/state/doc/`"), "{body}");
        assert!(body.contains("read the document as usual"), "{body}");
        assert!(!body.contains("known-state"), "{body}");
        assert!(!body.contains("dispatch"), "{body}");
    }

    /// Workflow discovery remains complete in the generated live map even
    /// though the fallback file does not duplicate it.
    #[test]
    fn the_live_map_teaches_every_workflow_verb_from_the_tree() {
        let map = render().expect("live map renders");
        let verbs = workflow_verbs().expect("workflow verbs resolve");
        assert_eq!(verbs.len(), WORKFLOW_VERBS.len());
        for v in &verbs {
            assert!(map.contains(&v.usage), "the live map lost `{}`", v.name);
        }
        assert!(
            map.contains("rift investigate <SEED> [options] --json"),
            "the live map must name the seed: {map}"
        );
        assert!(
            map.contains("rift dispatch --for-agent [options] --json"),
            "the live map must name the agent arm of dispatch: {map}"
        );
    }

    /// The hint this build actually ships, run through the guard it actually
    /// meets.
    ///
    /// The push-side chokepoint withholds a hint it does not like and takes the
    /// manifest's whole "detail on demand" line with it — silently, because
    /// withholding is the correct behavior and there is nobody to tell. Every
    /// other test of that line supplies its own literal hint, so all of them
    /// would keep passing while the shipped one lost its line. The failure is
    /// one bracket away: a future spelling with `--known-hash <sha256>` in it
    /// is refused by the charset allowlist, and the manifest would quietly stop
    /// naming the command that makes its own counters actionable.
    ///
    /// The near miss is worth pinning too. `rift recall diff-since --json`
    /// normalizes to `riftrecalldiffsincejson`, and the frame's header opener
    /// normalizes to `riftrecordeddelta`: seven characters of shared prefix
    /// before they part. A shorter opener would refuse the product's own hint.
    #[test]
    fn the_live_pull_hint_survives_the_push_guard() {
        let hint = manifest_pull_hint(SRC, HASH);
        let text = rift_recall_serve::render_delta_manifest(
            &[rift_recall_serve::ManifestEntry::path_only("src/main.rs")],
            0,
            &hint,
        )
        .expect("one entry renders");
        assert!(
            text.contains("Detail on demand"),
            "the manifest lost its detail line: {text}"
        );
        // Whole and unaltered: a hint the guard truncated or trimmed would
        // still leave the line, and would still be unrunnable.
        assert!(text.contains(&hint), "the hint was altered: {text}");
    }

    /// The locked artifact itself. Lifted out of the test body because it is
    /// DATA, not logic: inline, it put the assertion over the workspace's
    /// function-length ceiling, and an `#[allow]` there would have been a lint
    /// silenced to store a string.
    const GOLDEN: &str = "rift — provenance, recall, and cost for AI coding agents.

This repository is captured by Rift: every agent read, edit, and checkpoint
lands in an append-only ledger. Before re-reading files, ask Rift what actually
changed since the state you already hold.

Every verb below is one short-lived process, takes --json, and answers in one
shot. Nothing is interactive and nothing blocks you.

Answers use four states: known_unchanged (you already hold these bytes),
summary_included, exact_included, and available_by_handle (ask for it by
handle, with a reason). Responses are budgeted: content that does not fit is
omitted AND said to be omitted, never silently truncated. Exact retrieval is
policy-gated, requires a reason, and every serve is audited.

Start a turn with known-state, and pass the content hashes you already hold via
--known-hash so Rift can tell you what to skip.

Verbs:

  rift recall known-state --task <TEXT> --file <PATH> [options] --json
      Manifest first: what changed, what you already hold, what to retrieve
      --task <TEXT>            What you are trying to do right now (required)
      --file <PATH>            A file currently in focus; repeat for several (at least one required)
      --error <SIGNATURE>      An error signature currently in play; repeatable
      --known-hash <SHA256>    A content hash you already hold; repeatable (drives `known_unchanged`)
      --token-budget <TOKENS>  Token ceiling for the response (clamped to the per-turn budget)
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall diff-since [options] --json
      Changed vs unchanged files, given the content hashes you already hold
      --known-hash <SHA256>    A content hash you already hold; repeatable. With none, everything reads as changed — which is the honest answer for an agent holding nothing
      --token-budget <TOKENS>  Token ceiling for the response (clamped to the per-turn budget)
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall file-if-changed <PATH> [options] --json
      Is a file still what you last saw? (`unchanged` / `changed` / `invalid`.)
      <PATH>                   The repo-relative path to ask about
      --known-hash <SHA256>    A content hash you already hold for it; repeatable
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall changed-hunks <PATH> [options] --json
      The changed lines of a file since the revision you hold, with content
      <PATH>                   The repo-relative path to ask about (with `--known-hash`)
      --known-hash <SHA256>    The revision you hold: its content hash, or the leading 16+ hex of one
      --task <TEXT>            What you are trying to do right now (required unless you name a path and `--known-hash`)
      --file <PATH>            A file currently in focus; repeat for several (required unless you name a path and `--known-hash`)
      --error <SIGNATURE>      An error signature currently in play; repeatable
      --token-budget <TOKENS>  Token ceiling for the response (clamped to the per-turn budget)
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall read-summary <PATH> [options] --json
      A hash-bound summary — reported `stale`, never served, once its source moved
      <PATH>                   The repo-relative path to ask about
      --known-hash <SHA256>    A content hash you already hold for it; repeatable
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall retrieve-exact <HANDLE> --reason <TEXT> [options] --json
      Exact bytes by handle. Policy-gated; `--reason` is mandatory and audited
      <HANDLE>                 The handle to retrieve (e.g. `file:src/lib.rs@sha256:<hash>`)
      --reason <TEXT>          Why the exact bytes are needed. Mandatory, recorded in the serve audit
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall cost-hotspots [options] --json
      Where the context tokens go (descriptive awareness; never a directive)
      --top-n <N>              How many hotspots to report
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall context-pack --task <TEXT> --file <PATH> [options] --json
      One ordered, budgeted, position-aware evidence pack
      --task <TEXT>            What you are trying to do right now (required)
      --file <PATH>            A file currently in focus; repeat for several (at least one required)
      --error <SIGNATURE>      An error signature currently in play; repeatable
      --known-hash <SHA256>    A content hash you already hold; repeatable (drives `known_unchanged`)
      --token-budget <TOKENS>  Token ceiling for the response (clamped to the per-turn budget)
      --cross-branch <REASON>  Widen containment past this checkout's branch, with an audited reason

  rift recall doc-read <PATH> --json
      A document as Markdown: the sidecar Rift holds for a `.docx`, `.pdf`, spreadsheet or slide deck, instead of the file's own bytes
      <PATH>                   The document to read as Markdown (repo-relative or absolute)

  rift recall search <QUERY> [options] --json
      Which paths in this checkout a query is about, ranked by match and by what this family works on — instead of listing and reading your way to them
      <QUERY>                  What to look for: words matched against paths, with `*.rs` (extension), `/src/` (path segment) and `!tests/` (exclusion) constraints
      --content                Also return the matching lines from the tracked files that ranked (a literal, matched without regard to case)

Provenance verbs (who wrote it, under which prompt, at what cost):

  rift blame <PATH> [options] --json
      Blame a file: which checkpoint, session and agent explain each line
      <PATH>                   A file tracked in this worktree
      --line <N|N-M>           Answer for one line (`--line 12`) or an inclusive range (`--line 12-20`) instead of the whole file

  rift why <PATH[:LINE]> [options] --json
      One line's full story: who wrote it, under which prompt, at what cost
      <PATH[:LINE]>            A file tracked in this worktree, optionally with a line (`src/a.rs:42`)
      --line <N>               The line to explain (alternative to the `:LINE` suffix). One line only — `rift blame --line N-M` answers ranges

  rift checkpoint explain <ID|SHA> [options] --json
      Explain one checkpoint natively from its shard: metadata, digests, the redacted prompt, files, tokens/cost, attribution. No target = HEAD
      <ID|SHA>                 A checkpoint id (or unique prefix), a commit sha/ref, or nothing for HEAD's claimed checkpoint (a squash resolves to the latest by created-at)
      --full                   Also render the compact transcript
      --short                  Header, sessions and digest summary only

Workflow verbs (Rift compiles the evidence; the reasoning is yours):

  rift review [options] --json
      Compile the review evidence pack for the commits in scope. Rift gathers the facts; the verdict is your agent's
      --since <WHEN>           Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --until <WHEN>           Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --branch <BRANCH>        Scope to the checkpoints one branch's commits reach
      --profile <PROFILE>      Which sections and budget: `full` (default), `signals`, or `risk`

  rift investigate <SEED> [options] --json
      Compile an investigation's budgeted evidence pack; writes no file
      <SEED>                   The seed: a path, a checkpoint id, or `sha256:<hex>`
      --allow-untrusted-seed   Admit a non-identifier seed as a neutral digest (never verbatim)
      --finding <FINDING>      A finding from the `investigate` skill: `<kind>:<handle>::<text>`. Repeatable. Accepted findings are UNVERIFIED notes

  rift dispatch --for-agent [options] --json
      What happened in a window: checkpoints, sessions, attribution, cost, savings, risk — facts only, for your agent to reason over
      --since <WHEN>           Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --until <WHEN>           Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --branch <BRANCH>        Scope to the checkpoints one branch's commits reach, resolved from the `Rift-Checkpoint` trailers and the shards' own anchor commits
      --md                     Render the report as Markdown instead of plain text
      --for-agent              Emit a budgeted evidence pack (pack-spine shaped) instead of prose

  rift recap [options] --json
      Offline usage analytics: sessions, tokens, tool mix, serves, savings
      --since <WHEN>           Window start, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --until <WHEN>           Window end, inclusive: a UTC day (`YYYY-MM-DD`) or epoch milliseconds
      --agent                  Split by agent (the default)
      --model                  Split by model instead of by agent

This text is generated from the installed binary's own command tree,
so it always describes the version you are running.
";

    /// The rendered map is a locked artifact (a locked agent-help snapshot):
    /// it is what every skill file and the session pointer point
    /// at, so a change to it is a reviewed change to agent-facing guidance.
    #[test]
    fn agent_help_snapshot_is_locked() {
        assert_eq!(render().unwrap(), GOLDEN);
    }
}

#[cfg(test)]
mod goldens;
