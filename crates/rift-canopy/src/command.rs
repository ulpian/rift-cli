//! The closed slash grammar, v2 — as amended by the injected dispatcher.
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
    /// Recall Viewer checkpoint catalogue (column 2).
    Checkpoints,
    /// Recall Viewer selected-checkpoint timeline (column 3).
    Timeline,
    /// The command bar.
    Command,
}

/// Arguments accepted by the read-only Recall Viewer entry command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewerArgs {
    /// Optional checkpoint id/prefix or Git commit-ish/ref.
    pub target: Option<String>,
    /// Optional repository path. The viewer treats it as session-local.
    pub repo: Option<String>,
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

/// `profile` subcommands (palette parity with the profile menu —
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
    /// effective state (parallel to `New`).
    Duplicate(String),
    /// Pin the named profile as the launch view, or with `None` (the bare
    /// verb) unpin — launch follows last-selected again.
    Startup(Option<String>),
}

/// The splash-mode values `configure splash` accepts.
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
    /// Open the selected repository's Project Settings modal.
    Settings,
    /// Open or retarget the read-only Recall Viewer.
    Viewer(ViewerArgs),
    /// Move focus to a panel.
    Focus(PanelName),
    /// Manage profiles (the profile plane and its menu) — the menu's grammar as slash verbs.
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
    /// List the family's worktrees (name, branch, provisioned, prunable).
    Worktrees,
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
    /// Wire the selected worktree for every agent the family has enabled.
    Adopt,
}

/// The privilege tier a verb dispatches under.
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
            | Self::Settings
            | Self::Viewer(_)
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
            | Self::ProjectsPruneDryRun
            | Self::Worktrees => Tier::ReadOnly,
            Self::Init
            | Self::EnableAgent(_)
            | Self::DisableAgent(_)
            | Self::Configure(_)
            | Self::Rebuild
            | Self::Gc
            | Self::ProjectsPrune
            | Self::Adopt => Tier::Mutating,
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
    "settings",
    "viewer",
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
    "worktrees",
    "adopt",
    "init",
    "enable",
    "disable",
    "configure",
    "rebuild",
];

/// One help line per verb word — the `/help` overlay body (the suggest
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
                "settings" => "/settings",
                "viewer" => "/viewer [target] [--repo path]",
                "focus" => "/focus <projects|detail|graphs|checkpoints|timeline|command>",
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
                "worktrees" => "/worktrees",
                "adopt" => "/adopt",
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
/// The match below is the verb table: a bare verb is spelled inline, and a
/// verb that takes arguments hands them to its own parser, so the table reads
/// as the grammar's word list in [`VERB_WORDS`] order.
///
/// # Errors
/// [`ParseError`] with the offending span, a reason, and (for an unknown
/// verb) the nearest-verb hint.
pub fn parse(input: &str) -> Result<Verb, ParseError> {
    refuse_oversized(input)?;
    let tokens = tokenize_input(input)?;
    let Some((verb_tok, rest)) = tokens.split_first() else {
        return Err(ParseError::at((0, input.len()), "type a verb — try /help"));
    };
    let word = verb_tok.text.to_ascii_lowercase();
    match word.as_str() {
        "select" => Ok(Verb::Select(project_ref(rest.first()))),
        "add" => Ok(Verb::Add),
        "remove" => Ok(Verb::Remove(project_ref(rest.first()))),
        "relabel" => parse_relabel(verb_tok, rest),
        "settings" => Ok(Verb::Settings),
        "viewer" => parse_viewer(rest),
        "focus" => parse_focus(verb_tok, rest),
        "profile" => parse_profile(verb_tok, rest),
        "help" => Ok(Verb::Help),
        "quit" | "q" => Ok(Verb::Quit),
        "status" => Ok(Verb::Status),
        "memory" => parse_memory(verb_tok, rest),
        "checkpoint" => parse_checkpoint(verb_tok, rest),
        "session" => parse_session(verb_tok, rest),
        "explain" => required_arg(
            verb_tok,
            rest,
            "a checkpoint reference",
            "/explain <checkpoint>",
        )
        .map(Verb::Explain),
        "doctor" => Ok(Verb::Doctor),
        "verify" => Ok(Verb::Verify),
        "gc" => parse_gc(rest),
        "projects" => parse_projects(rest),
        // Read-only and in-process; the mutating twin below confirms first.
        // Both are BARE verbs on purpose: the palette acts on the selected
        // project, so a path argument would be a second way to name a target
        // and the two could disagree.
        "worktrees" => Ok(Verb::Worktrees),
        "adopt" => Ok(Verb::Adopt),
        "init" | "setup" => Ok(Verb::Init),
        "enable" => required_arg(
            verb_tok,
            rest,
            "an agent (e.g. claude-code)",
            "/enable <agent>",
        )
        .map(Verb::EnableAgent),
        "disable" => required_arg(
            verb_tok,
            rest,
            "an agent (e.g. claude-code)",
            "/disable <agent>",
        )
        .map(Verb::DisableAgent),
        "configure" => parse_configure(verb_tok, rest),
        "rebuild" => Ok(Verb::Rebuild),
        _ => Err(unknown_verb(verb_tok, &word)),
    }
}

/// Refuse a buffer past [`MAX_INPUT_LEN`] before tokenizing, so a paste bomb
/// costs one length check.
fn refuse_oversized(input: &str) -> Result<(), ParseError> {
    if input.len() <= MAX_INPUT_LEN {
        return Ok(());
    }
    // A span must name a SLICE of the input it describes. `MAX_INPUT_LEN`
    // is a BYTE count, so it can land inside a multi-byte character, and
    // anything that slices the input at the span to draw a caret would
    // panic there. Walk down to the nearest boundary — `input.len()` is
    // always one, so the end needs no adjustment.
    let mut start = MAX_INPUT_LEN;
    while start > 0 && !input.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    Err(ParseError::at(
        (start, input.len()),
        format!("input is longer than {MAX_INPUT_LEN} bytes — trim it"),
    ))
}

/// Tokenize the buffer past one optional leading slash, with every span —
/// token and error alike — shifted back onto the original input.
fn tokenize_input(input: &str) -> Result<Vec<Token>, ParseError> {
    // Strip one leading slash (its span offset is preserved by tokenizing the
    // original string and skipping the slash character positionally).
    let body_off = usize::from(input.starts_with('/'));
    let body = input.get(body_off..).unwrap_or_default();
    // The tokenizer's spans are into `body`, so they are shifted back onto the
    // original input below. Its ERROR span needs the same shift and was not
    // getting it: `/"x` reported the unterminated quote at 0..3 of a 4-byte
    // input, one byte short at both ends — and since the shift is what puts the
    // end back on a character boundary, the span could also land INSIDE a
    // multi-byte character, which panics anything that slices at it.
    let mut tokens = tokenize(body).map_err(|e| ParseError {
        span: (
            e.span.0.saturating_add(body_off),
            e.span.1.saturating_add(body_off),
        ),
        ..e
    })?;
    for t in &mut tokens {
        t.start += body_off;
        t.end += body_off;
    }
    Ok(tokens)
}

/// The unknown-verb error, carrying the nearest known verb as its hint.
fn unknown_verb(verb_tok: &Token, word: &str) -> ParseError {
    let mut err = ParseError::at(verb_tok.span(), format!("unknown verb `{}`", verb_tok.text));
    err.hint = nearest_verb(word);
    err
}

/// The one required argument of a single-argument verb (`explain`, `enable`,
/// `disable`); its absence is reported at the end of the verb.
fn required_arg(
    verb_tok: &Token,
    rest: &[Token],
    what: &str,
    usage: &str,
) -> Result<String, ParseError> {
    match rest.first() {
        Some(t) => Ok(t.text.clone()),
        None => Err(missing(verb_tok, what, usage)),
    }
}

/// `/relabel [project] <new label…>`.
fn parse_relabel(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    // The LAST token(s) form the label; with 1 arg the selection is relabeled.
    let verb = match rest.len() {
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
            Verb::Relabel(project_ref(rest.first()), label)
        }
    };
    Ok(verb)
}

/// `/viewer [target] [--repo <path>]`, the options in any order.
fn parse_viewer(rest: &[Token]) -> Result<Verb, ParseError> {
    let mut target = None;
    let mut repo = None;
    let mut i = 0;
    while let Some(token) = rest.get(i) {
        if token.text == "--repo" {
            let Some(path) = rest.get(i + 1) else {
                return Err(missing(
                    token,
                    "a repository path",
                    "/viewer [target] --repo <path>",
                ));
            };
            if repo.replace(path.text.clone()).is_some() {
                return Err(ParseError::at(
                    token.span(),
                    "--repo may be supplied only once",
                ));
            }
            i += 2;
            continue;
        }
        if token.text.starts_with('-') {
            return Err(ParseError::at(
                token.span(),
                "unknown viewer option — only --repo <path>",
            ));
        }
        if target.replace(token.text.clone()).is_some() {
            return Err(ParseError::at(
                token.span(),
                "viewer accepts at most one target",
            ));
        }
        i += 1;
    }
    Ok(Verb::Viewer(ViewerArgs { target, repo }))
}

/// `/focus <panel>`.
fn parse_focus(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb =
        match arg(0).map(|t| t.text.to_ascii_lowercase()) {
            Some(p) => match p.as_str() {
                "projects" => Verb::Focus(PanelName::Projects),
                "detail" => Verb::Focus(PanelName::Detail),
                "graphs" => Verb::Focus(PanelName::Graphs),
                "checkpoints" => Verb::Focus(PanelName::Checkpoints),
                "timeline" => Verb::Focus(PanelName::Timeline),
                "command" => Verb::Focus(PanelName::Command),
                _ => return Err(ParseError::at(
                    arg(0).map_or((0, 0), Token::span),
                    "unknown panel — projects | detail | graphs | checkpoints | timeline | command",
                )),
            },
            None => {
                return Err(missing(
                    verb_tok,
                    "a panel",
                    "/focus <projects|detail|graphs|checkpoints|timeline|command>",
                ))
            }
        };
    Ok(verb)
}

/// `/profile <sub> [name]` — the profile menu's grammar as slash verbs.
fn parse_profile(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
                // no-argument profile verb.
                "startup" => Verb::Profile(ProfileVerb::Startup(arg(1).map(|t| t.text.clone()))),
                _ => return Err(ParseError::at(
                    arg(0).map_or((0, 0), Token::span),
                    "unknown profile verb — new | rename | switch | delete | duplicate | startup",
                )),
            }
        }
        None => {
            return Err(missing(
                verb_tok,
                "a profile verb",
                "/profile <new|rename|switch|delete|duplicate|startup> [name]",
            ))
        }
    };
    Ok(verb)
}

/// `/memory <sub> [arg]` — the recall plane's read-only CLI verbs.
fn parse_memory(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
    };
    Ok(verb)
}

/// `/checkpoint [list|show <id>]`; the bare verb lists.
fn parse_checkpoint(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
    };
    Ok(verb)
}

/// `/session [list|show <id>]`; the bare verb lists.
fn parse_session(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
    };
    Ok(verb)
}

/// `/gc [--dry-run]`.
fn parse_gc(rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.as_str()) {
        Some("--dry-run") => Verb::GcDryRun,
        Some(_) => {
            return Err(ParseError::at(
                arg(0).map_or((0, 0), Token::span),
                "unknown gc argument — only --dry-run",
            ))
        }
        None => Verb::Gc,
    };
    Ok(verb)
}

/// `/projects [list|prune [--dry-run]]`; the bare verb lists.
fn parse_projects(rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
    };
    Ok(verb)
}

/// `/configure [splash <mode>]`; the bare verb shows the current config.
fn parse_configure(verb_tok: &Token, rest: &[Token]) -> Result<Verb, ParseError> {
    let arg = |i: usize| rest.get(i);
    let verb = match arg(0).map(|t| t.text.to_ascii_lowercase()) {
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
        // The bare verb is the unpin — the one no-argument
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
        assert_eq!(parse("/viewer"), Ok(Verb::Viewer(ViewerArgs::default())));
        assert_eq!(
            parse("/viewer abc123 --repo \"/tmp/my repo\""),
            Ok(Verb::Viewer(ViewerArgs {
                target: Some("abc123".into()),
                repo: Some("/tmp/my repo".into()),
            }))
        );
        assert!(parse("/viewer a b").is_err());
        assert!(parse("/viewer --repo").is_err());
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

        let err = parse("/relabel \"open").unwrap_err();
        assert!(err.reason.contains("unterminated"));
    }

    /// The two defects the strengthened span property found, pinned by example
    /// so they cannot come back as "the property is probabilistic".
    ///
    /// `ParseError::span` is documented as "byte range into the ORIGINAL
    /// input (suitable for underline rendering)", and both failures were that
    /// contract being false.
    #[test]
    fn an_error_span_always_names_a_slice_of_the_original_input() {
        // (1) The tokenizer runs on the body, AFTER the leading slash is
        // stripped, and its error span was returned unshifted — so every
        // slash-prefixed command underlined one byte to the left.
        let input = "/relabel \"open";
        let err = parse(input).unwrap_err();
        assert!(err.reason.contains("unterminated"));
        assert_eq!(
            input.get(err.span.0..err.span.1),
            Some("\"open"),
            "the span must underline the quote it is complaining about"
        );

        // (2) …and the same missing shift, plus a byte-counted cap, could put
        // an end INSIDE a multi-byte character. Slicing there is a panic, not
        // a cosmetic error.
        for input in ["/\"¡", &format!("/x {}", "é".repeat(300))] {
            let err = parse(input).unwrap_err();
            assert!(
                input.get(err.span.0..err.span.1).is_some(),
                "span {:?} does not slice a {}-byte input",
                err.span,
                input.len()
            );
        }
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
        fn parse_is_total_over_arbitrary_input(input in "(?s).{0,600}") {
            match parse(&input) {
                Ok(_) => {}
                Err(e) => {
                    prop_assert!(e.span.0 <= e.span.1);
                    // The bound here was `input.len().max(MAX_INPUT_LEN +
                    // input.len())`, whose `max` is ALWAYS its second term — so
                    // it asserted `span.1 <= 512 + len`, which no span this
                    // parser can produce could exceed. A span DESCRIBES this
                    // input, so the real claim is that it names a slice of it,
                    // and the caret that renders it needs both ends to be char
                    // boundaries or the slice panics.
                    prop_assert!(
                        e.span.1 <= input.len(),
                        "span {:?} runs past the end of a {}-byte input",
                        e.span, input.len()
                    );
                    prop_assert!(
                        input.is_char_boundary(e.span.0) && input.is_char_boundary(e.span.1),
                        "span {:?} does not name a slice of the input", e.span
                    );
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
