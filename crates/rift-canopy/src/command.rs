//! The closed slash grammar, v2 (ADR-0015 as amended by ADR-0021).
//!
//! One typed, closed verb enum across four tiers: dashboard verbs that map to
//! the panels' own typed effects; read-only CLI delegation; mutating CLI
//! delegation (confirm-gated); and `hooks`, which is **structurally absent** —
//! no [`Verb`] variant can express it, so no input can reach it. Parsing is a
//! hand-rolled tokenizer with minimal quoting: shell metacharacters are
//! ordinary argument text (the free-text surface is inert by construction),
//! the buffer is length-capped, and every failure is a span-carrying
//! [`ParseError`] rendered inline under the input. Pure logic — no terminal,
//! no process, no I/O — so it is fully testable and fuzzable.

/// The maximum input length the parser accepts (a paste bomb caps here;
/// everything past the cap is refused with a span at the cap).
pub const MAX_INPUT_LEN: usize = 512;

/// A reference to a project: the current selection (no argument), a registered
/// name, or a 1-based row index (`#2` or `2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectRef {
    /// The currently selected row.
    Selected,
    /// By registered label (exact match; the reducer resolves it).
    Name(String),
    /// By 1-based row index.
    Index(usize),
}

/// A panel name for `/focus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelName {
    /// Column 1.
    Projects,
    /// Column 2.
    Detail,
    /// Column 3.
    Graphs,
    /// The command bar.
    Command,
}

/// `memory` subcommands (read-only delegation; mirrors the CLI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryVerb {
    /// What changed since a checkpoint.
    DiffSince(String),
    /// Does a file differ from its last captured read.
    FileIfChanged(String),
    /// The files consuming the most context tokens.
    CostHotspots(Option<u32>),
    /// The measured reread/avoidable-context report.
    RereadReport,
}

/// `checkpoint` subcommands (read-only delegation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointVerb {
    /// List checkpoints.
    List,
    /// Show one checkpoint.
    Show(String),
}

/// `session` subcommands (read-only delegation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionVerb {
    /// List sessions.
    List,
    /// Show one session.
    Show(String),
}

/// `profile` subcommands (ADR-0037 §6: palette parity with the profile menu —
/// two surfaces, one grammar). Names are resolved against the live profiles in
/// the state layer; every act is the same typed effect the menu emits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileVerb {
    /// Create a profile (copying the active profile's project list).
    New(String),
    /// Rename the active profile.
    Rename(String),
    /// Switch to the named profile (the whole view follows).
    Switch(String),
    /// Delete the named profile (guards refuse the active and the last).
    Delete(String),
    /// Duplicate the ACTIVE profile under a new name — the full clone of its
    /// effective state (ADR-0045 §1; parallel to `New`).
    Duplicate(String),
    /// Pin the named profile as the launch view, or with `None` (the bare
    /// verb) unpin — launch follows last-selected again (ADR-0045 §2).
    Startup(Option<String>),
}

/// The splash-mode values `configure splash` accepts (ADR-0014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashMode {
    /// Play on every launch.
    Always,
    /// Play on the first run only.
    FirstRun,
    /// Never play.
    Never,
}

impl SplashMode {
    /// The canonical lowercase token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::FirstRun => "first-run",
            Self::Never => "never",
        }
    }
}

/// `configure` subcommands (mutating delegation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigureVerb {
    /// Print the current config.
    Show,
    /// Set when the splash plays (writes only the splash key of the global
    /// canopy preferences).
    Splash(SplashMode),
}

/// The closed verb set — every palette action across all four tiers. `hooks`
/// has no variant: the agent/git wire entry is structurally unreachable from
/// free text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    // ---- dashboard tier (typed panel effects; never exceeds a panel) ------
    /// Select a project row.
    Select(ProjectRef),
    /// Register the launch directory (explicit mutation; never automatic).
    Add,
    /// Remove (hide) a project — a superseding event, history kept.
    Remove(ProjectRef),
    /// Relabel a project.
    Relabel(ProjectRef, String),
    /// Flip a feature toggle for the selected project.
    Toggle(String),
    /// Reinstall a feature for the selected project.
    Reinstall(String),
    /// Move focus to a panel.
    Focus(PanelName),
    /// Manage profiles (ADR-0036/0037) — the menu's grammar as slash verbs.
    Profile(ProfileVerb),
    /// Show the palette help (the full catalog).
    Help,
    /// Quit the dashboard.
    Quit,
    // ---- read-only delegation (in-process dispatcher → results overlay) ---
    /// One-shot capture/projection health.
    Status,
    /// The recall plane's human surface.
    Memory(MemoryVerb),
    /// Inspect checkpoints.
    Checkpoint(CheckpointVerb),
    /// Inspect sessions.
    Session(SessionVerb),
    /// Explain a checkpoint reference.
    Explain(String),
    /// Capture-backend + memory-chain readiness.
    Doctor,
    /// Re-derive the hash chain and cross-check the projection.
    Verify,
    /// Report what gc would prune, deleting nothing.
    GcDryRun,
    /// List the registered projects (path, label, presence).
    ProjectsList,
    /// Report which registered rows prune would sweep, writing nothing.
    ProjectsPruneDryRun,
    // ---- mutating delegation (confirm-in-UI, then the dispatcher) ---------
    /// Scaffold `.rift/`, install hooks, init the projection.
    Init,
    /// Wire an agent's hooks.
    EnableAgent(String),
    /// Remove an agent's hook wiring.
    DisableAgent(String),
    /// Configure policy / the splash mode.
    Configure(ConfigureVerb),
    /// Rebuild the projection from the checkpoint branch.
    Rebuild,
    /// Prune transient refs/objects (never the canonical chain).
    Gc,
    /// Sweep path-missing registry rows (superseding removed events).
    ProjectsPrune,
}

/// The privilege tier a verb dispatches under (ADR-0021).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Typed panel effects only.
    Dashboard,
    /// The injected dispatcher; renders in the results overlay; no confirm.
    ReadOnly,
    /// Confirm-in-UI first, then the dispatcher.
    Mutating,
}

impl Verb {
    /// The tier this verb dispatches under.
    #[must_use]
    pub fn tier(&self) -> Tier {
        match self {
            Self::Select(_)
            | Self::Add
            | Self::Remove(_)
            | Self::Relabel(..)
            | Self::Toggle(_)
            | Self::Reinstall(_)
            | Self::Focus(_)
            | Self::Profile(_)
            | Self::Help
            | Self::Quit => Tier::Dashboard,
            Self::Status
            | Self::Memory(_)
            | Self::Checkpoint(_)
            | Self::Session(_)
            | Self::Explain(_)
            | Self::Doctor
            | Self::Verify
            | Self::GcDryRun
            | Self::ProjectsList
            | Self::ProjectsPruneDryRun => Tier::ReadOnly,
            Self::Init
            | Self::EnableAgent(_)
            | Self::DisableAgent(_)
            | Self::Configure(_)
            | Self::Rebuild
            | Self::Gc
            | Self::ProjectsPrune => Tier::Mutating,
        }
    }
}

/// Every first-token verb word the grammar accepts — the catalog's sync
/// anchor (exactly one suggest entry per word, proven by test) and the
/// nearest-verb hint vocabulary. `q` is an alias of `quit`, not a word.
pub const VERB_WORDS: &[&str] = &[
    "select",
    "add",
    "remove",
    "relabel",
    "toggle",
    "reinstall",
    "focus",
    "profile",
    "help",
    "quit",
    "status",
    "memory",
    "checkpoint",
    "session",
    "explain",
    "doctor",
    "verify",
    "gc",
    "projects",
    "init",
    "enable",
    "disable",
    "configure",
    "rebuild",
];

/// One help line per verb word — the `/help` overlay body (the W1 suggest
/// catalog builds richer entries on the same word list).
#[must_use]
pub fn help_lines() -> Vec<String> {
    VERB_WORDS
        .iter()
        .map(|w| {
            let usage = match *w {
                "select" => "/select [project]",
                "add" => "/add",
                "remove" => "/remove [project]",
                "relabel" => "/relabel [project] <label>",
                "toggle" => "/toggle <feature>",
                "reinstall" => "/reinstall <feature>",
                "focus" => "/focus <projects|detail|graphs|command>",
                "profile" => "/profile <new|rename|switch|delete|duplicate|startup> [name]",
                "help" => "/help",
                "quit" => "/quit (or /q)",
                "status" => "/status",
                "memory" => "/memory <diff-since|file-if-changed|cost-hotspots|reread-report>",
                "checkpoint" => "/checkpoint [list|show <id>]",
                "session" => "/session [list|show <id>]",
                "explain" => "/explain <checkpoint>",
                "doctor" => "/doctor",
                "verify" => "/verify",
                "gc" => "/gc [--dry-run]",
                "projects" => "/projects [list|prune [--dry-run]]",
                "init" => "/init (alias /setup)",
                "enable" => "/enable <agent>",
                "disable" => "/disable <agent>",
                "configure" => "/configure [splash <always|first-run|never>]",
                "rebuild" => "/rebuild",
                _ => "",
            };
            usage.to_owned()
        })
        .collect()
}

/// A parse failure: the byte span of the offending region in the (possibly
/// slash-stripped) input, the reason, and — for an unknown verb — the nearest
/// known verb as a hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Byte range into the ORIGINAL input (suitable for underline rendering).
    pub span: (usize, usize),
    /// What went wrong, human-phrased.
    pub reason: String,
    /// The nearest known verb, when the failure was an unknown verb.
    pub hint: Option<&'static str>,
}

impl ParseError {
    fn at(span: (usize, usize), reason: impl Into<String>) -> Self {
        Self {
            span,
            reason: reason.into(),
            hint: None,
        }
    }
}

/// One token with its byte span in the original input.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

impl Token {
    fn span(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}

/// Tokenize with minimal quoting: whitespace separates; a `"…"` region groups
/// (quotes stripped, no escape processing — a metacharacter is always literal
/// text). An unterminated quote is a span error. Total over arbitrary input.
fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start: Option<usize> = None;
    let mut quote_open: Option<usize> = None;
    for (i, ch) in input.char_indices() {
        if let Some(qs) = quote_open {
            if ch == '"' {
                quote_open = None;
            } else {
                cur.push(ch);
            }
            // the token continues either way; a quote never closes a token
            let _ = qs;
            continue;
        }
        match ch {
            '"' => {
                quote_open = Some(i);
                if start.is_none() {
                    start = Some(i);
                }
            }
            c if c.is_whitespace() => {
                if let Some(s) = start.take() {
                    out.push(Token {
                        text: std::mem::take(&mut cur),
                        start: s,
                        end: i,
                    });
                }
            }
            c => {
                if start.is_none() {
                    start = Some(i);
                }
                cur.push(c);
            }
        }
    }
    if let Some(qs) = quote_open {
        return Err(ParseError::at(
            (qs, input.len()),
            "unterminated quote — close the \" or remove it",
        ));
    }
    if let Some(s) = start {
        out.push(Token {
            text: cur,
            start: s,
            end: input.len(),
        });
    }
    Ok(out)
}

/// Levenshtein distance (small strings only — verb words).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        if let Some(slot) = cur.first_mut() {
            *slot = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let del = prev
                .get(j + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let ins = cur.get(j).copied().unwrap_or(usize::MAX).saturating_add(1);
            let sub = prev
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(cost);
            if let Some(slot) = cur.get_mut(j + 1) {
                *slot = del.min(ins).min(sub);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.last().copied().unwrap_or(usize::MAX)
}

/// The nearest verb word within edit distance 2, for the unknown-verb hint.
#[must_use]
pub fn nearest_verb(word: &str) -> Option<&'static str> {
    VERB_WORDS
        .iter()
        .map(|v| (edit_distance(word, v), *v))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, v)| (*d, *v))
        .map(|(_, v)| v)
}

/// Parse a project-reference argument; absent → the current selection.
fn project_ref(tok: Option<&Token>) -> ProjectRef {
    let Some(t) = tok else {
        return ProjectRef::Selected;
    };
    let text = t.text.strip_prefix('#').unwrap_or(&t.text);
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = text.parse::<usize>() {
            if n >= 1 {
                return ProjectRef::Index(n);
            }
        }
    }
    ProjectRef::Name(t.text.clone())
}

/// A required-argument error at the end of the last token (or the verb).
fn missing(after: &Token, what: &str, usage: &str) -> ParseError {
    ParseError::at(
        (after.end, after.end),
        format!("missing {what} — usage: {usage}"),
    )
}

/// Parse one palette buffer into a [`Verb`]. Total over arbitrary input:
/// every failure is a span-carrying error, never a panic. A leading `/` is
/// optional; the buffer is capped at [`MAX_INPUT_LEN`].
///
/// # Errors
/// [`ParseError`] with the offending span, a reason, and (for an unknown
/// verb) the nearest-verb hint.
#[allow(clippy::too_many_lines)] // one linear verb table; a match arm per verb
pub fn parse(input: &str) -> Result<Verb, ParseError> {
    if input.len() > MAX_INPUT_LEN {
        return Err(ParseError::at(
            (MAX_INPUT_LEN, input.len()),
            format!("input is longer than {MAX_INPUT_LEN} bytes — trim it"),
        ));
    }
    // Strip one leading slash (its span offset is preserved by tokenizing the
    // original string and skipping the slash character positionally).
    let body_off = usize::from(input.starts_with('/'));
    let body = input.get(body_off..).unwrap_or_default();
    let mut tokens = tokenize(body)?;
    for t in &mut tokens {
        t.start += body_off;
        t.end += body_off;
    }
    let Some(verb_tok) = tokens.first() else {
        return Err(ParseError::at((0, input.len()), "type a verb — try /help"));
    };
    let rest = tokens.get(1..).unwrap_or_default();
    let arg = |i: usize| rest.get(i);
    let word = verb_tok.text.to_ascii_lowercase();

    let verb = match word.as_str() {
        "select" => Verb::Select(project_ref(arg(0))),
        "add" => Verb::Add,
        "remove" => Verb::Remove(project_ref(arg(0))),
        "relabel" => {
            // /relabel [project] <new label…> — the LAST token(s) form the
            // label; with 1 arg the selection is relabeled.
            match rest.len() {
                0 => {
                    return Err(missing(
                        verb_tok,
                        "the new label",
                        "/relabel [project] <label>",
                    ))
                }
                1 => Verb::Relabel(
                    ProjectRef::Selected,
                    rest.first().map(|t| t.text.clone()).unwrap_or_default(),
                ),
                _ => {
                    let label = rest
                        .get(1..)
                        .unwrap_or_default()
                        .iter()
                        .map(|t| t.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    Verb::Relabel(project_ref(arg(0)), label)
                }
            }
        }
        "toggle" => match arg(0) {
            Some(t) => Verb::Toggle(t.text.clone()),
            None => return Err(missing(verb_tok, "a feature key", "/toggle <feature>")),
        },
        "reinstall" => match arg(0) {
            Some(t) => Verb::Reinstall(t.text.clone()),
            None => return Err(missing(verb_tok, "a feature key", "/reinstall <feature>")),
        },
        "focus" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(p) => match p.as_str() {
                "projects" => Verb::Focus(PanelName::Projects),
                "detail" => Verb::Focus(PanelName::Detail),
                "graphs" => Verb::Focus(PanelName::Graphs),
                "command" => Verb::Focus(PanelName::Command),
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown panel — projects | detail | graphs | command",
                    ))
                }
            },
            None => {
                return Err(missing(
                    verb_tok,
                    "a panel",
                    "/focus <projects|detail|graphs|command>",
                ))
            }
        },
        "profile" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => {
                let name = |usage: &str| match arg(1) {
                    Some(t) => Ok(t.text.clone()),
                    None => Err(missing(arg(0).unwrap_or(verb_tok), "a profile name", usage)),
                };
                match sub.as_str() {
                    "new" => Verb::Profile(ProfileVerb::New(name("/profile new <name>")?)),
                    "rename" => Verb::Profile(ProfileVerb::Rename(name("/profile rename <name>")?)),
                    "switch" => Verb::Profile(ProfileVerb::Switch(name("/profile switch <name>")?)),
                    "delete" => Verb::Profile(ProfileVerb::Delete(name("/profile delete <name>")?)),
                    "duplicate" => Verb::Profile(ProfileVerb::Duplicate(name(
                        "/profile duplicate <new-name>",
                    )?)),
                    // Bare `/profile startup` is the unpin — the one
                    // no-argument profile verb (ADR-0045 §6).
                    "startup" => Verb::Profile(ProfileVerb::Startup(
                        arg(1).map(|t| t.text.clone()),
                    )),
                    _ => {
                        return Err(ParseError::at(
                            arg(0).map_or((0, 0), Token::span),
                            "unknown profile verb — new | rename | switch | delete | duplicate | startup",
                        ))
                    }
                }
            }
            None => {
                return Err(missing(
                    verb_tok,
                    "a profile verb",
                    "/profile <new|rename|switch|delete|duplicate|startup> [name]",
                ))
            }
        },
        "help" => Verb::Help,
        "quit" | "q" => Verb::Quit,
        "status" => Verb::Status,
        "memory" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => match sub.as_str() {
                "diff-since" => match arg(1) {
                    Some(t) => Verb::Memory(MemoryVerb::DiffSince(t.text.clone())),
                    None => {
                        return Err(missing(
                            arg(0).unwrap_or(verb_tok),
                            "a checkpoint id",
                            "/memory diff-since <checkpoint>",
                        ))
                    }
                },
                "file-if-changed" => match arg(1) {
                    Some(t) => Verb::Memory(MemoryVerb::FileIfChanged(t.text.clone())),
                    None => {
                        return Err(missing(
                            arg(0).unwrap_or(verb_tok),
                            "a file path",
                            "/memory file-if-changed <path>",
                        ))
                    }
                },
                "cost-hotspots" => {
                    let top = arg(1).and_then(|t| t.text.parse::<u32>().ok());
                    Verb::Memory(MemoryVerb::CostHotspots(top))
                }
                "reread-report" => Verb::Memory(MemoryVerb::RereadReport),
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown memory verb — diff-since | file-if-changed | \
                         cost-hotspots | reread-report",
                    ))
                }
            },
            None => {
                return Err(missing(
                    verb_tok,
                    "a memory verb",
                    "/memory <diff-since|file-if-changed|cost-hotspots|reread-report>",
                ))
            }
        },
        "checkpoint" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => match sub.as_str() {
                "list" => Verb::Checkpoint(CheckpointVerb::List),
                "show" => match arg(1) {
                    Some(t) => Verb::Checkpoint(CheckpointVerb::Show(t.text.clone())),
                    None => {
                        return Err(missing(
                            arg(0).unwrap_or(verb_tok),
                            "a checkpoint id",
                            "/checkpoint show <id>",
                        ))
                    }
                },
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown checkpoint verb — list | show",
                    ))
                }
            },
            None => Verb::Checkpoint(CheckpointVerb::List),
        },
        "session" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => match sub.as_str() {
                "list" => Verb::Session(SessionVerb::List),
                "show" => match arg(1) {
                    Some(t) => Verb::Session(SessionVerb::Show(t.text.clone())),
                    None => {
                        return Err(missing(
                            arg(0).unwrap_or(verb_tok),
                            "a session id",
                            "/session show <id>",
                        ))
                    }
                },
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown session verb — list | show",
                    ))
                }
            },
            None => Verb::Session(SessionVerb::List),
        },
        "explain" => match arg(0) {
            Some(t) => Verb::Explain(t.text.clone()),
            None => {
                return Err(missing(
                    verb_tok,
                    "a checkpoint reference",
                    "/explain <checkpoint>",
                ))
            }
        },
        "doctor" => Verb::Doctor,
        "verify" => Verb::Verify,
        "gc" => match arg(0).map(|t| t.text.as_str()) {
            Some("--dry-run") => Verb::GcDryRun,
            Some(_) => {
                return Err(ParseError::at(
                    arg(0).map_or((0, 0), Token::span),
                    "unknown gc argument — only --dry-run",
                ))
            }
            None => Verb::Gc,
        },
        "projects" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => match sub.as_str() {
                "list" => Verb::ProjectsList,
                "prune" => match arg(1).map(|t| t.text.as_str()) {
                    Some("--dry-run") => Verb::ProjectsPruneDryRun,
                    Some(_) => {
                        return Err(ParseError::at(
                            arg(1).map_or((0, 0), Token::span),
                            "unknown prune argument — only --dry-run",
                        ))
                    }
                    None => Verb::ProjectsPrune,
                },
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown projects verb — list | prune",
                    ))
                }
            },
            None => Verb::ProjectsList,
        },
        "init" | "setup" => Verb::Init,
        "enable" => match arg(0) {
            Some(t) => Verb::EnableAgent(t.text.clone()),
            None => {
                return Err(missing(
                    verb_tok,
                    "an agent (e.g. claude-code)",
                    "/enable <agent>",
                ))
            }
        },
        "disable" => match arg(0) {
            Some(t) => Verb::DisableAgent(t.text.clone()),
            None => {
                return Err(missing(
                    verb_tok,
                    "an agent (e.g. claude-code)",
                    "/disable <agent>",
                ))
            }
        },
        "configure" => match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(sub) => match sub.as_str() {
                "splash" => match arg(1).map(|t| t.text.to_ascii_lowercase()) {
                    Some(m) => match m.as_str() {
                        "always" => Verb::Configure(ConfigureVerb::Splash(SplashMode::Always)),
                        "first-run" => Verb::Configure(ConfigureVerb::Splash(SplashMode::FirstRun)),
                        "never" => Verb::Configure(ConfigureVerb::Splash(SplashMode::Never)),
                        _ => {
                            return Err(ParseError::at(
                                arg(1).map_or((0, 0), Token::span),
                                "unknown splash mode — always | first-run | never",
                            ))
                        }
                    },
                    None => {
                        return Err(missing(
                            arg(0).unwrap_or(verb_tok),
                            "a splash mode",
                            "/configure splash <always|first-run|never>",
                        ))
                    }
                },
                _ => {
                    return Err(ParseError::at(
                        arg(0).map_or((0, 0), Token::span),
                        "unknown configure verb — splash",
                    ))
                }
            },
            None => Verb::Configure(ConfigureVerb::Show),
        },
        "rebuild" => Verb::Rebuild,
        _ => {
            let mut err =
                ParseError::at(verb_tok.span(), format!("unknown verb `{}`", verb_tok.text));
            err.hint = nearest_verb(&word);
            return Err(err);
        }
    };
    Ok(verb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn profile_duplicate_and_startup_parse_including_bare_startup() {
        assert_eq!(
            parse("/profile duplicate lab-copy"),
            Ok(Verb::Profile(ProfileVerb::Duplicate("lab-copy".into())))
        );
        assert_eq!(
            parse("/profile startup lab"),
            Ok(Verb::Profile(ProfileVerb::Startup(Some("lab".into()))))
        );
        // The bare verb is the unpin (ADR-0045 §6) — the one no-argument
        // profile verb, never a parse error.
        assert_eq!(
            parse("/profile startup"),
            Ok(Verb::Profile(ProfileVerb::Startup(None)))
        );
        // Duplicate still requires its new name.
        assert!(parse("/profile duplicate").is_err());
    }

    #[test]
    fn dashboard_verbs_parse_with_project_refs() {
        assert_eq!(parse("/select"), Ok(Verb::Select(ProjectRef::Selected)));
        assert_eq!(parse("/select 2"), Ok(Verb::Select(ProjectRef::Index(2))));
        assert_eq!(parse("select #3"), Ok(Verb::Select(ProjectRef::Index(3))));
        assert_eq!(
            parse("/select rift-cli"),
            Ok(Verb::Select(ProjectRef::Name("rift-cli".into())))
        );
        assert_eq!(
            parse("/relabel \"my repo\""),
            Ok(Verb::Relabel(ProjectRef::Selected, "my repo".into()))
        );
        assert_eq!(
            parse("/relabel 2 new name"),
            Ok(Verb::Relabel(ProjectRef::Index(2), "new name".into()))
        );
        assert_eq!(parse("/q"), Ok(Verb::Quit));
        assert_eq!(parse("quit"), Ok(Verb::Quit));
    }

    #[test]
    fn delegation_tiers_parse_and_classify() {
        assert_eq!(parse("/status").map(|v| v.tier()), Ok(Tier::ReadOnly));
        assert_eq!(parse("/gc --dry-run"), Ok(Verb::GcDryRun));
        assert_eq!(parse("/gc").map(|v| v.tier()), Ok(Tier::Mutating));
        assert_eq!(
            parse("/projects prune --dry-run"),
            Ok(Verb::ProjectsPruneDryRun)
        );
        assert_eq!(
            parse("/projects prune").map(|v| v.tier()),
            Ok(Tier::Mutating)
        );
        assert_eq!(
            parse("/memory cost-hotspots 7"),
            Ok(Verb::Memory(MemoryVerb::CostHotspots(Some(7))))
        );
        assert_eq!(
            parse("/configure splash never"),
            Ok(Verb::Configure(ConfigureVerb::Splash(SplashMode::Never)))
        );
        assert_eq!(parse("/setup"), Ok(Verb::Init));
    }

    #[test]
    fn hooks_is_structurally_absent() {
        // No variant exists; the word parses as an unknown verb.
        let err = parse("/hooks git post-commit").unwrap_err();
        assert!(err.reason.contains("unknown verb"));
        assert!(!VERB_WORDS.contains(&"hooks"));
    }

    #[test]
    fn metacharacters_are_literal_text() {
        // A pasted weird path can never become execution — it is an argument.
        let v = parse("/select $(rm -rf /)").unwrap();
        assert_eq!(v, Verb::Select(ProjectRef::Name("$(rm".into())));
        let v = parse("/relabel \"a; rm -rf | x\"").unwrap();
        assert_eq!(
            v,
            Verb::Relabel(ProjectRef::Selected, "a; rm -rf | x".into())
        );
    }

    #[test]
    fn span_errors_point_at_the_offense() {
        let err = parse("/statsu").unwrap_err();
        assert_eq!(err.span, (1, 7), "the verb token span");
        assert_eq!(err.hint, Some("status"), "nearest-verb hint");

        let err = parse("/toggle").unwrap_err();
        assert_eq!(err.span.0, err.span.1, "missing-arg span is a caret");
        assert!(err.reason.contains("usage"));

        let err = parse("/relabel \"open").unwrap_err();
        assert!(err.reason.contains("unterminated"));
    }

    #[test]
    fn oversized_input_is_capped_with_a_span() {
        let big = format!("/select {}", "x".repeat(MAX_INPUT_LEN));
        let err = parse(&big).unwrap_err();
        assert_eq!(err.span.0, MAX_INPUT_LEN);
        assert!(err.reason.contains("longer than"));
    }

    #[test]
    fn empty_input_asks_for_a_verb() {
        assert!(parse("").is_err());
        assert!(parse("/").is_err());
        assert!(parse("   ").is_err());
    }

    proptest! {
        /// Parse is TOTAL: any input yields Ok or a spanned Err — never a
        /// panic, and every error span lies within the input bounds.
        #[test]
        fn parse_is_total_over_arbitrary_input(input in ".{0,600}") {
            match parse(&input) {
                Ok(_) => {}
                Err(e) => {
                    prop_assert!(e.span.0 <= e.span.1);
                    prop_assert!(e.span.1 <= input.len().max(MAX_INPUT_LEN + input.len()));
                }
            }
        }

        /// Every VERB_WORDS entry parses to SOMETHING (with minimal args) —
        /// the word list and the grammar cannot drift apart.
        #[test]
        fn every_verb_word_is_reachable(idx in 0..VERB_WORDS.len()) {
            let word = VERB_WORDS[idx];
            // Give each verb its minimal argument shape.
            let line = match word {
                "toggle" | "reinstall" => format!("/{word} fff"),
                "explain" => format!("/{word} abc123"),
                "enable" | "disable" => format!("/{word} claude-code"),
                "focus" => format!("/{word} projects"),
                "relabel" => format!("/{word} newname"),
                "memory" => format!("/{word} reread-report"),
                "profile" => format!("/{word} switch work"),
                _ => format!("/{word}"),
            };
            prop_assert!(parse(&line).is_ok(), "verb `{word}` must parse: {line}");
        }
    }
}
