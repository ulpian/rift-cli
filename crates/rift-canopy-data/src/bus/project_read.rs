//! One project's read: its live status from the family's projection and its
//! session caches, and the rich detail the active project's card shows.
//!
//! A separate module because the status classification is the fact every row
//! of the dashboard rests on, and its truth table (`derive_status`) is pure —
//! this module only gathers the inputs, read-only, and degrades every failure
//! to a status rather than a panic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use rift_agent_install::detect::DetectionEnvironment;
use rift_agent_install::health::{self, Facts, Remedy, Severity};
use rift_canopy::{
    derive_status, CaptureSummary, DetailView, EntryKind, ModelProvenance, ModelUsage,
    ProjectStatus, ProjectionState, SessionsRead, StatusInputs,
};
use rift_git::FamilyMember;
use rift_store::{projection_db_for_family_under, CanonicalStore, SqliteStore};
use tokio::runtime::Runtime;

use crate::feeds::project_family;
use crate::store::runtime;

use super::now_ms;
use super::settings_view::{family_dir, hooks_installed_here, project_settings_view_under};
use super::watermark::Watermark;

/// What one read of a project yields: its live status, its `{cursor, epoch}`
/// watermark, and — for a family row — which members its `Running` count came
/// from.
pub(super) struct ProjectRead {
    pub(super) status: ProjectStatus,
    pub(super) mark: Option<Watermark>,
    pub(super) running_members: Vec<String>,
}

impl ProjectRead {
    /// A read that names no running member: every early return, and every
    /// member row (whose count is its own and needs no breakdown).
    const fn plain(status: ProjectStatus, mark: Option<Watermark>) -> Self {
        Self {
            status,
            mark,
            running_members: Vec::new(),
        }
    }
}

/// The family-shaped half of a project read: whose sessions this row counts,
/// whether it is an adopted member, and which members are running.
struct FamilyRead {
    /// Whether the path is inside a git repository at all.
    repository: bool,
    sessions: SessionsRead,
    provisioned: Option<bool>,
    running_members: Vec<String>,
    /// Every member, whose session journals the health read gathers.
    members: Vec<FamilyMember>,
}

/// Where one family's integration-health facts are read from: every input
/// [`Facts::gather`] takes but the sessions, which are read from `members`.
///
/// The Codex home is a field, not a process read inside the gather, so a test
/// points it at a temporary directory and never at the machine's own.
struct HealthSources<'a> {
    /// The checkout whose hook files are read.
    repo: &'a Path,
    /// The family's manifest directory, which holds its binary of record.
    family_dir: PathBuf,
    /// The family's projection; opened read-only, and only when it exists.
    projection_db: PathBuf,
    /// Every member of the family, whose session journals are read.
    members: &'a [FamilyMember],
    /// Codex's home, when there is one to read Codex's trust from.
    codex_home: Option<PathBuf>,
}

/// Every file whose change can move the health read's answer, stamped by
/// modification time, and the refresh window the read falls in.
///
/// The detectors are functions of these files and a clock value: the hook
/// files and the binary of record, Codex's trust record, every member's
/// session journal, and the projection (whose file changes whenever its
/// watermark does). The window bounds what no stamp sees — a wired binary
/// deleted, or a session ageing out of the alarm window — to one refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HealthStamp {
    files: Vec<Option<SystemTime>>,
    journals: Vec<JournalStamp>,
    window: i64,
}

/// One member's session journal: the directory's own stamp, how many caches
/// it holds and the newest of them, so a cache rewritten in place moves it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalStamp {
    dir: Option<SystemTime>,
    caches: usize,
    newest: Option<SystemTime>,
}

/// How long a cached health answer may stand with no stamped file moving.
const HEALTH_REFRESH_MS: i64 = 30_000;

/// The last health answer per checkout, with the stamp it was computed
/// under. Keyed by checkout path and bounded by the registry's rows; a
/// process-wide map because the status read is a free function the worker
/// calls, and the answer is a pure function of the stamped files.
static HEALTH_CACHE: Mutex<BTreeMap<PathBuf, (HealthStamp, bool)>> = Mutex::new(BTreeMap::new());

impl FamilyRead {
    /// A path with no family at all — not a repository, or one whose common dir
    /// cannot be read. No sessions to find, and "is this member provisioned?"
    /// is not a question that applies.
    const fn none() -> Self {
        Self {
            repository: false,
            sessions: SessionsRead::Ok(None),
            provisioned: None,
            running_members: Vec::new(),
            members: Vec::new(),
        }
    }

    /// Whether `repo`'s family has an error-severity integration finding as
    /// of `now_ms`; `false` for a path with no family.
    fn integration_failing(
        &self,
        root: &Path,
        repo: &Path,
        family: Option<&rift_git::Family>,
        now_ms: i64,
    ) -> bool {
        let Some(family) = family else {
            return false;
        };
        let sources = HealthSources::from_process(root, repo, family, &self.members);
        cached_integration_failing(&sources, now_ms)
    }
}

impl<'a> HealthSources<'a> {
    /// The sources for `repo`, a checkout of `family`, under the state root
    /// `root`, with the process's own Codex home.
    fn from_process(
        root: &Path,
        repo: &'a Path,
        family: &rift_git::Family,
        members: &'a [FamilyMember],
    ) -> Self {
        Self {
            repo,
            family_dir: family_dir(family),
            projection_db: projection_db_for_family_under(root, family),
            members,
            codex_home: rift_agent_install::native::codex_home_from_process(),
        }
    }
}

impl HealthStamp {
    /// Stamp every file `sources` reads, as of `now_ms`.
    fn read(sources: &HealthSources<'_>, now_ms: i64) -> Self {
        let hook_files = rift_shadow_hooks::AdapterRegistry::with_builtin()
            .agents()
            .iter()
            .filter_map(|agent| rift_agent_install::native::hook_file(agent.as_str()))
            .map(|file| sources.repo.join(file))
            .collect::<Vec<_>>();
        let projection = sources.projection_db.as_os_str();
        let mut wal = projection.to_owned();
        wal.push("-wal");
        let others = [
            rift_shadow_hooks::binary_of_record_path(&sources.family_dir),
            sources.projection_db.clone(),
            PathBuf::from(wal),
        ];
        let codex = sources
            .codex_home
            .iter()
            .map(|home| home.join("config.toml"));
        let files = hook_files
            .into_iter()
            .chain(others)
            .chain(codex)
            .map(|path| modified(&path))
            .collect();
        let journals = sources
            .members
            .iter()
            .map(|member| {
                JournalStamp::read(&crate::session::member_sessions(
                    &member.git_dir,
                    &member.work_tree,
                ))
            })
            .collect();
        Self {
            files,
            journals,
            window: now_ms.div_euclid(HEALTH_REFRESH_MS),
        }
    }
}

impl JournalStamp {
    fn read(dir: &Path) -> Self {
        let mut caches = 0;
        let mut newest = None;
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            caches += 1;
            let stamp = entry.metadata().and_then(|meta| meta.modified()).ok();
            newest = newest.max(stamp);
        }
        Self {
            dir: modified(dir),
            caches,
            newest,
        }
    }
}

/// Read a project's live status **and** its `{cursor, epoch}` watermark from
/// the **repository family's** projection in a single open — read-only, scoped
/// by the family's `repo_id`. The gathered facts (repo presence, projection
/// readiness, the live-session read, capture presence, whether this row is an
/// adopted member) are fed to the pure [`derive_status`] truth table, so the
/// classification is testable without a store; any failure degrades to
/// an `Error`/`NotInitialized` status, never a panic.
///
/// **The row decides which session read it gets** (the family view).
/// A **linked worktree** is a member row: its sessions are the ones credited to
/// it by the worktree discriminator ([`crate::session::family_sessions`], never
/// its own directory alone), and it additionally answers whether it has been
/// provisioned — an initialized family whose member carries no
/// `.rift/state/native-integrations.json` has nothing in that checkout wired to
/// Rift, which is `NotAdopted`. Anything else is the **family row**: its
/// `Running` counts every member's sessions, and it names them, so the number
/// can be read back to the checkouts it came from.
///
/// A member whose directory is gone but whose administrative entry survives —
/// git's `prunable` — canonicalises to nothing and takes the first branch:
/// `Error(RepoMissing)`, whose hint on a worktree row says prunable.
///
/// **A projection is not an install.** Before any capture-shaped reading, a
/// family whose binary of record names no live binary answers
/// `SetupIncomplete` — the same answer the settings page gives, so the card
/// and its modal cannot disagree about whether there is setup left to do.
///
/// The watermark is `Some` iff the projection is open and readable, and `None`
/// iff it is absent/unreadable — the same `None` the change observer treats as
/// [`ChangeKind::Unavailable`].
pub(super) fn read_project(root: &Path, path: &str) -> ProjectRead {
    let dir = PathBuf::from(path);
    let now_ms = now_ms();

    // A registered path that no longer resolves → the repo is gone. This is a
    // question about the PATH, not about identity, so it stays a canonicalize:
    // a directory that is present but is not a repository is present.
    if std::fs::canonicalize(&dir).is_err() {
        return ProjectRead::plain(
            status_of(
                false,
                ProjectionState::Uninitialized,
                SessionsRead::Ok(None),
                false,
                None,
            ),
            None,
        );
    }
    if dir.join(".rift/state/project-setup.json").is_file() {
        return ProjectRead::plain(ProjectStatus::SetupIncomplete, None);
    }
    let family_read = read_family(&dir, now_ms);
    let sessions = family_read.sessions;
    let provisioned = family_read.provisioned;
    // The projection is the repository family's (family identity): a card opened on
    // a linked worktree reads the family's store, and a directory that is not a
    // repository has no family — which reads exactly like no projection,
    // because family identity mints no per-path store to give it one.
    let family = project_family(&dir);
    let db = family
        .as_ref()
        .map(|family| projection_db_for_family_under(root, family));
    let Some(db) = db.filter(|db| db.exists()) else {
        let projection = projection_absent(family_read.repository, &dir);
        return ProjectRead::plain(
            status_of(true, projection, sessions, false, provisioned),
            None,
        );
    };
    // The projection is not the install. An agent hook opens the store on
    // first contact, so a family nothing was ever wired in carries a
    // projection; the card reads it the way the settings page does, and the
    // add flow's act is the one this status asks for.
    if family
        .as_ref()
        .is_some_and(|family| !hooks_installed_here(family))
    {
        return ProjectRead::plain(ProjectStatus::SetupIncomplete, None);
    }
    // The projection exists: open once, read the watermark + freshest event.
    let Some((mark, last_ts)) = open_and_read(&db) else {
        return ProjectRead::plain(
            status_of(
                true,
                ProjectionState::Unavailable,
                sessions,
                false,
                provisioned,
            ),
            None,
        );
    };
    // Only a readable projection reaches the rows the finding outranks, so
    // only here is the health read worth its cost.
    let integration_failing = family_read.integration_failing(root, &dir, family.as_ref(), now_ms);
    ProjectRead {
        status: derive_status(&StatusInputs {
            repo_present: true,
            projection: ProjectionState::Ready,
            sessions,
            has_captures: last_ts.is_some(),
            provisioned,
            integration_failing,
        }),
        mark: Some(mark),
        running_members: family_read.running_members,
    }
}

/// Every integration-health remedy for the checkout at `dir`, for Project
/// Settings to print under the adapter rows. Empty outside a repository.
pub(super) fn project_remedies(root: &Path, dir: &Path) -> Vec<Remedy> {
    let Ok(paths) = rift_git::discover(dir) else {
        return Vec::new();
    };
    let Some(family) = project_family(dir) else {
        return Vec::new();
    };
    let roster = crate::session::roster(&paths);
    let sources = HealthSources::from_process(root, dir, &family, &roster.members);
    integration_remedies(&sources, &DetectionEnvironment::from_process(), now_ms())
}

/// Build the rich column-2 [`DetailView`] for the active project: its effective
/// settings (from `.rift/config.toml`) plus the models and capture counts from its
/// own projection. Any unreadable part degrades to empty/default — never a panic.
/// The `status` is the already-derived one, so the detail and the list agree.
pub(super) fn build_detail(
    root: &Path,
    path: &str,
    status: ProjectStatus,
    running_members: Vec<String>,
) -> DetailView {
    let dir = PathBuf::from(path);
    let label = dir
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let (models, capture) = read_projection_detail(root, &dir).unwrap_or_default();
    // A `.git` FILE marks a linked worktree; a directory is a repository root.
    let kind = if dir.join(".git").is_file() {
        EntryKind::Worktree
    } else {
        EntryKind::Repo
    };
    DetailView {
        path: path.to_string(),
        label,
        kind,
        status,
        models,
        capture,
        running_members,
        project_settings: Box::new(project_settings_view_under(root, &dir)),
    }
}

/// How a path with no projection under this machine's root reads.
///
/// `.rift/` is committed configuration and proves nothing about this machine,
/// so a checkout carrying it is configured elsewhere, never initialized; a
/// directory outside any repository has no family and can never hold one.
fn projection_absent(repository: bool, dir: &Path) -> ProjectionState {
    if !repository {
        ProjectionState::NotRepository
    } else if dir.join(".rift").is_dir() {
        ProjectionState::Configured
    } else {
        ProjectionState::Uninitialized
    }
}

/// Resolve `dir`'s family and read the session facts its row needs.
fn read_family(dir: &Path, now_ms: i64) -> FamilyRead {
    let Ok(paths) = rift_git::discover(dir) else {
        return FamilyRead::none();
    };
    let roster = crate::session::roster(&paths);
    let read = crate::session::family_sessions(&roster.members, now_ms);
    if paths.is_linked() {
        // A member row: its own credited sessions, and the adoption question.
        let sessions = read
            .per_member
            .get(roster.vantage)
            .map_or(SessionsRead::Ok(None), |detail| detail.read);
        return FamilyRead {
            repository: true,
            sessions,
            provisioned: Some(is_provisioned(&paths.work_tree)),
            running_members: Vec::new(),
            members: roster.members,
        };
    }
    let running_members = roster
        .members
        .iter()
        .zip(&read.per_member)
        .filter(|(_, detail)| matches!(detail.read, SessionsRead::Ok(Some(_))))
        .map(|(member, _)| member_label(member))
        .collect();
    FamilyRead {
        repository: true,
        sessions: read.family.read,
        provisioned: None,
        running_members,
        members: roster.members,
    }
}

/// Whether a member checkout has ever been provisioned: the native-integration
/// ownership manifest `rift_agent_install::native::enable` writes, which is the
/// same file `post-checkout`'s fast path stats (family wiring and hook custody).
///
/// A **file-presence** check on purpose: it is stable whatever the adoption
/// API grows into, it costs one `stat` on a path the bus already holds, and a
/// manifest that exists but cannot be parsed still proves that something once
/// provisioned this checkout — which is the question the status asks.
fn is_provisioned(work_tree: &Path) -> bool {
    work_tree
        .join(".rift/state/native-integrations.json")
        .is_file()
}

/// A member's name for the family header: `main` for the main work tree, the
/// administrative name for a linked one (worktree provenance's vocabulary, so a reader
/// never has to know this repository's folder name to recognise main).
fn member_label(member: &rift_git::FamilyMember) -> String {
    if member.is_main {
        "main".to_owned()
    } else {
        member.name.clone()
    }
}

/// Assemble the [`StatusInputs`] and run the pure truth table, for every read
/// that stops before a readable projection: no integration finding is read
/// there, because every such status outranks one.
fn status_of(
    repo_present: bool,
    projection: ProjectionState,
    sessions: SessionsRead,
    has_captures: bool,
    provisioned: Option<bool>,
) -> ProjectStatus {
    derive_status(&StatusInputs {
        repo_present,
        projection,
        sessions,
        has_captures,
        provisioned,
        integration_failing: false,
    })
}

/// Open the projection read-only and return `(watermark, most-recent
/// occurred_at)`, or `None` if it cannot be opened/read
/// (→ [`ProjectionState::Unavailable`]).
fn open_and_read(db: &Path) -> Option<(Watermark, Option<i64>)> {
    let store = SqliteStore::open(db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    // The freshest event's transaction time (0 rows → None, not an error).
    let last_ts = rt
        .block_on(store.query_opt_i64(
            "SELECT occurred_at_ms FROM captured_event ORDER BY occurred_at_ms DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    Some((Watermark { cursor, epoch }, last_ts))
}

/// Read a project's models + capture counts from its **family's** projection
/// (read-only, scoped by `repo_id`). `None` if the projection is
/// missing/unreadable — or if the path has no family to key one.
fn read_projection_detail(root: &Path, dir: &Path) -> Option<(Vec<ModelUsage>, CaptureSummary)> {
    let db = projection_db_for_family_under(root, &project_family(dir)?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    Some((
        read_model_usage(&store, &rt),
        read_capture_summary(&store, &rt),
    ))
}

/// The models a project has used, most-used first: calls and the later
/// append-only observations. Runtime facts supersede configured fallback
/// facts; empty legacy session values are ignored.
fn read_model_usage(store: &SqliteStore, rt: &Runtime) -> Vec<ModelUsage> {
    let mut counts: BTreeMap<String, (u64, ModelProvenance)> = BTreeMap::new();
    absorb_model_calls(store, rt, &mut counts);
    absorb_model_observations(store, rt, &mut counts);
    absorb_session_headlines(store, rt, &mut counts);
    let mut models: Vec<ModelUsage> = counts
        .into_iter()
        .map(|(model, (calls, source))| ModelUsage {
            model,
            calls,
            source,
        })
        .collect();
    models.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));
    models
}

/// Every model call, counted per model — a runtime fact.
fn absorb_model_calls(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
) {
    let Ok(rows) =
        rt.block_on(store.query("SELECT model, COUNT(*) FROM model_call GROUP BY model", &[]))
    else {
        return;
    };
    for row in rows {
        let (Ok(model), Ok(n)) = (row.get_text(0), row.get_i64(1)) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        counts.insert(
            model,
            (u64::try_from(n).unwrap_or(0), ModelProvenance::Runtime),
        );
    }
}

/// Every observed model, with its provenance: a runtime observation upgrades
/// a configured one, never the reverse.
fn absorb_model_observations(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
) {
    let Ok(rows) = rt.block_on(store.query(
        "SELECT model, source FROM model_observation WHERE model <> ''",
        &[],
    )) else {
        return;
    };
    for row in rows {
        let (Ok(model), Ok(source)) = (row.get_text(0), row.get_text(1)) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        let provenance = if source == "configured_client" {
            ModelProvenance::Configured
        } else {
            ModelProvenance::Runtime
        };
        counts
            .entry(model)
            .and_modify(|(_, old)| {
                if provenance == ModelProvenance::Runtime {
                    *old = provenance;
                }
            })
            .or_insert((0, provenance));
    }
}

/// Every session's headline model, so a model that made no recorded call
/// still appears — with zero calls.
fn absorb_session_headlines(
    store: &SqliteStore,
    rt: &Runtime,
    counts: &mut BTreeMap<String, (u64, ModelProvenance)>,
) {
    let Ok(rows) = rt.block_on(store.query("SELECT DISTINCT model_primary FROM session", &[]))
    else {
        return;
    };
    for row in rows {
        let Ok(model) = row.get_text(0) else {
            continue;
        };
        if model.trim().is_empty() {
            continue;
        }
        counts.entry(model).or_insert((0, ModelProvenance::Runtime));
    }
}

/// The capture counts and the freshest checkpoint's time.
fn read_capture_summary(store: &SqliteStore, rt: &Runtime) -> CaptureSummary {
    let count = |t: &str| u64::try_from(rt.block_on(store.count(t)).unwrap_or(0)).unwrap_or(0);
    CaptureSummary {
        events: count("captured_event"),
        sessions: count("session"),
        checkpoints: count("checkpoint"),
        last_checkpoint_at_ms: rt
            .block_on(store.query_opt_i64(
                "SELECT created_at FROM checkpoint ORDER BY created_at DESC LIMIT 1",
                &[],
            ))
            .ok()
            .flatten(),
    }
}

/// [`integration_failing`], recomputed only when its stamp moves: the poll
/// reads a project far more often than any input to its findings changes,
/// and the full read costs as much again as the rest of the status read.
fn cached_integration_failing(sources: &HealthSources<'_>, now_ms: i64) -> bool {
    let stamp = HealthStamp::read(sources, now_ms);
    let cached = HEALTH_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(sources.repo).cloned());
    match cached {
        Some((cached_stamp, failing)) if cached_stamp == stamp => return failing,
        _ => (),
    }
    let failing = integration_failing(sources, now_ms);
    if let Ok(mut cache) = HEALTH_CACHE.lock() {
        cache.insert(sources.repo.to_path_buf(), (stamp, failing));
    }
    failing
}

/// Whether any finding over the family's facts is an error. Warnings never
/// count: nothing is being lost while they hold, so they do not turn the card.
///
/// Asks [`health::assess`] and not for remedies: the status needs no
/// sentence, and naming a surface detects installs, which the poll must not
/// pay for.
fn integration_failing(sources: &HealthSources<'_>, now_ms: i64) -> bool {
    with_health_facts(sources, |facts| {
        health::assess(facts, now_ms)
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    })
}

/// Every finding over the family's facts with its remedy, the same list
/// `rift status` and `rift doctor` print.
fn integration_remedies(
    sources: &HealthSources<'_>,
    env: &DetectionEnvironment,
    now_ms: i64,
) -> Vec<Remedy> {
    with_health_facts(sources, |facts| health::remedies(facts, env, now_ms))
}

/// Gather the family's health facts, read-only, and hand them to `use_facts`.
///
/// The projection is opened read-only, never through the status read's own
/// open, so this read cannot create, migrate or lock anything; the hook files,
/// `binary.json` and the session journals are parsed in place.
fn with_health_facts<T>(sources: &HealthSources<'_>, use_facts: impl FnOnce(&Facts<'_>) -> T) -> T {
    let sessions = crate::session::family_journal(sources.members);
    // A read-only open creates nothing: an absent projection fails it.
    let projection = SqliteStore::open_read_only(&sources.projection_db).ok();
    let facts = Facts::gather(
        sources.repo,
        &sources.family_dir,
        sources.codex_home.as_deref(),
        &sessions,
        projection.as_ref(),
    );
    use_facts(&facts)
}

/// A path's modification time; `None` when it is absent or unreadable, which
/// is itself a stamp: a file appearing moves it.
fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use rift_canopy::StatusError;
    use rift_git::repo_id_for;
    use rift_store::{projection_db_under, Value};

    use super::super::fixtures::{
        family_dir_of, live_session, make_project, path_of, provision, record_hooks_installed,
        runtime, two_member_family,
    };
    use super::super::watermark::read_watermark_pair;
    use super::*;

    /// Open an existing project's projection store.
    fn open_proj(root: &Path, proj: &Path) -> SqliteStore {
        let repo = repo_id_for(proj).unwrap();
        SqliteStore::open(projection_db_under(root, &repo)).unwrap()
    }

    /// Seed a `session` row (headline model).
    fn seed_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("claude-code"),
                Value::text(model),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `model_call` row.
    fn seed_model_call(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("t"),
                Value::text(model),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `checkpoint` row at `created_at`.
    fn seed_checkpoint(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        id: &str,
        created_at: i64,
    ) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(created_at),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// The detail of an idle project with no running members.
    fn detail_of(root: &Path, path: &str) -> DetailView {
        build_detail(root, path, ProjectStatus::Idle, Vec::new())
    }

    /// The status half of [`read_project`] — the read surface under test.
    fn status(root: &Path, path: &str) -> ProjectStatus {
        read_project(root, path).status
    }

    /// **The dashboard half of family identity.** A card opened on a *linked worktree*
    /// reads the family's projection — the one the main work tree owns — and
    /// therefore shows the repository's capture history instead of the blank
    /// the member's own path used to key.
    ///
    /// Measured on this machine before the change: from `~/rift-worktrees/
    /// canopy` the dashboard reported 0 checkpoints / 0 sessions while the
    /// family held 58,864 events. The negative assertion is the load-bearing
    /// one — no per-path projection is created for the member — because
    /// counting the member's own rows would pass under either rule.
    #[test]
    fn a_member_path_reads_the_family_projection() {
        let root = tempfile::tempdir().unwrap();
        // A family: a main work tree with a seeded projection, and a linked
        // worktree that has never been captured into.
        let main = make_project(root.path(), "main", 3);
        let main_git = main.join(".git");
        let member = root.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
        // Adopted, so this test keeps stating what it is about: the family view
        // gives an UNprovisioned member `NotAdopted`, which would mask the
        // projection read this test exists to pin; the unadopted arm is
        // `an_unadopted_member_reads_not_adopted` below.
        provision(&member);

        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Idle,
            "a member reads the family's captured history, not an empty store"
        );
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            status(root.path(), &member.to_string_lossy()),
            "either member's vantage reports the one repository"
        );

        // The watermark the change observer polls comes from the same store.
        assert_eq!(
            read_watermark_pair(root.path(), &member.to_string_lossy()),
            read_watermark_pair(root.path(), &main.to_string_lossy()),
        );

        // And nothing minted a second projection for the member's own path —
        // the defect, not merely its symptom.
        let per_path = projection_db_under(
            root.path(),
            &repo_id_for(&member).expect("the member path canonicalizes"),
        );
        assert!(
            !per_path.exists(),
            "a member must not get a projection keyed by its own path"
        );
    }

    #[test]
    fn read_status_missing_repo_for_nonexistent_path() {
        let root = tempfile::tempdir().unwrap();
        let s = status(root.path(), "/definitely/not/a/real/path");
        assert_eq!(s, ProjectStatus::Error(StatusError::RepoMissing));
    }

    #[test]
    fn read_status_not_initialized_without_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("fresh");
        std::fs::create_dir_all(&proj).unwrap();
        rift_git::test_support::fake_repo(&proj).unwrap();
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::NotInitialized);

        // A directory outside any repository has no family to project and
        // reads as exactly that — never as a repository awaiting `rift init`.
        let plain = root.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let s = status(root.path(), &plain.to_string_lossy());
        assert_eq!(s, ProjectStatus::NotRepository);
    }

    #[test]
    fn read_status_idle_when_projection_has_capture_data() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "captured", 3);
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::Idle);
    }

    #[test]
    fn read_is_read_only_leaves_row_count_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "ro", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);

        let _ = status(root.path(), &proj.to_string_lossy());

        // The bus never writes: the row count is exactly what we seeded.
        let store = SqliteStore::open(&db).unwrap();
        assert_eq!(
            runtime().block_on(store.count("captured_event")).unwrap(),
            2
        );
    }

    #[test]
    fn scope_is_per_project_projection() {
        // Two projects have independent projections; reading one never reflects
        // the other's capture data.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 5);
        let b = root.path().join("b");
        std::fs::create_dir_all(&b).unwrap();
        rift_git::test_support::fake_repo(&b).unwrap();
        assert_eq!(
            status(root.path(), &a.to_string_lossy()),
            ProjectStatus::Idle
        );
        assert_eq!(
            status(root.path(), &b.to_string_lossy()),
            ProjectStatus::NotInitialized
        );
    }

    // --- status derivation from the live-session cache ---------------------

    /// Write `<proj>/.git/rift-sessions/<name>.json` with `contents` (the
    /// hermetic stand-in for the capture hooks' cache writer).
    ///
    /// The journal lives inside a gitdir, so the fixture makes one: creating
    /// the session directory alone left `<proj>/.git` a bare directory that
    /// hardened discovery refuses (family identity), and the status these tests
    /// read is derived from the repository the reader discovers.
    fn write_session_cache(proj: &Path, name: &str, contents: &str) {
        rift_git::test_support::fake_repo(proj).unwrap();
        let dir = proj.join(".git").join("rift-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), contents).unwrap();
    }

    /// A live cache body updated "now" (the reader's staleness window is
    /// generous; now_ms comes from the wall clock inside read_project).
    fn live_cache_json() -> String {
        format!(
            r#"{{"schema":1,"session_id":"s1","state":"active","agent":"claude-code",
                 "current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{}}}"#,
            super::now_ms()
        )
    }

    #[test]
    fn a_member_with_a_fresh_session_reads_running_and_the_family_aggregates_it() {
        // the family view, in one fixture because they are one fact
        // read at two scopes. The member's session cache sits under its own
        // administrative gitdir, which the family row has never looked in:
        // before this, a repository whose work happens in worktrees reported
        // `Idle` on the row the developer actually watches.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        provision(&member);
        let member_git = main.join(".git/worktrees/member");
        let now = super::now_ms();
        live_session(&member_git, "in-member", &member, now);

        assert!(
            matches!(
                status(root.path(), &member.to_string_lossy()),
                ProjectStatus::Running {
                    active_sessions: 1,
                    ..
                }
            ),
            "the member row counts the session driven in it"
        );
        assert!(
            matches!(
                status(root.path(), &main.to_string_lossy()),
                ProjectStatus::Running {
                    active_sessions: 1,
                    ..
                }
            ),
            "and the family row aggregates it, from a registry it is not in"
        );

        // A second session, this one in main: the family sums, and neither
        // member is credited with the other's.
        live_session(&main.join(".git"), "in-main", &main, now);
        assert!(matches!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 2,
                ..
            }
        ));
        assert!(matches!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 1,
                ..
            }
        ));

        // The family row names the members its count came from, so a badge of
        // 2 can be read back to the checkouts that produced it; a member row
        // names none, because its count is its own.
        let family = read_project(root.path(), &main.to_string_lossy());
        assert_eq!(family.running_members, vec!["main", "member"]);
        assert!(read_project(root.path(), &member.to_string_lossy())
            .running_members
            .is_empty());
    }

    #[test]
    fn an_unadopted_member_reads_not_adopted() {
        // The member of an initialized family that `rift worktree adopt` has
        // never touched: no adapter in it is wired, so nothing it does is
        // captured. It is NOT `SetupIncomplete`, which would mean
        // "resume the wizard", and there is no plan file here to resume — and
        // it is not `NotInitialized`, because the family IS initialized.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::NotAdopted
        );
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Idle,
            "the family row is never told a member's adoption state"
        );

        // Even with a live session and captured history in the family, the
        // answer is the same: the checkout is not wired, so the counts that
        // reach it are not its own doing.
        live_session(
            &main.join(".git/worktrees/member"),
            "s",
            &member,
            super::now_ms(),
        );
        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::NotAdopted
        );

        // Adoption is one file, and the status follows it immediately.
        provision(&member);
        assert!(matches!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Running { .. }
        ));
    }

    #[test]
    fn a_gone_member_reads_repo_missing_prunable() {
        // `git worktree remove`'s other half: the checkout is deleted, the
        // administrative entry survives, and git calls it prunable. The row
        // says the checkout is missing, and the hint on a worktree row is the
        // one act that fixes it — reported, never performed for the operator.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        provision(&member);
        std::fs::remove_dir_all(&member).unwrap();

        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Error(StatusError::RepoMissing)
        );
        assert_eq!(
            ProjectStatus::Error(StatusError::RepoMissing).hint(EntryKind::Worktree),
            Some("checkout gone \u{2014} run: git worktree prune")
        );
        // The family is unharmed by a member it can no longer read, and the
        // administrative entry is still there for `git worktree prune`.
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Idle
        );
        assert!(main.join(".git/worktrees/member").is_dir());
    }

    #[test]
    fn a_family_row_is_never_asked_whether_it_was_adopted() {
        // `rift init` provisions the main work tree, so the question is a
        // category error there: a repository row with no member manifest must
        // keep reading its own status, never `NotAdopted`.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "solo", 1);
        assert!(!proj.join(".rift/state/native-integrations.json").exists());
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn live_session_cache_makes_status_running() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "live", 1);
        write_session_cache(&proj, "s1", &live_cache_json());
        assert!(matches!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 1,
                ..
            }
        ));
    }

    #[test]
    fn corrupt_session_cache_is_a_distinct_error() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "bad", 1);
        write_session_cache(&proj, "s1", "not a valid cache file");
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Error(StatusError::CorruptSessionCache)
        );
    }

    #[test]
    fn absent_session_cache_leaves_status_idle() {
        // No cache → no live session → old capture data reads as Idle (and
        // never Running: the recency proxy is gone).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet", 1);
        rift_git::test_support::fake_repo(&proj).unwrap();
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn ended_session_cache_reads_idle_not_running() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "done", 1);
        write_session_cache(
            &proj,
            "s1",
            &format!(
                r#"{{"schema":1,"session_id":"s1","state":"ended","updated_at_ms":{}}}"#,
                super::now_ms()
            ),
        );
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    // --- the detail view -----------------------------------------------------

    #[test]
    fn aggregates_distinct_models_with_counts() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "m", 1);
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_model_call(&store, &rt, "c1", "A");
        seed_model_call(&store, &rt, "c2", "A");
        seed_model_call(&store, &rt, "c3", "B");
        seed_session(&store, &rt, "s1", "A"); // already seen
        seed_session(&store, &rt, "s2", "C"); // headline-only, 0 calls
        seed_session(&store, &rt, "s3", ""); // legacy unknown, never displayed

        let d = detail_of(root.path(), &path_of(&proj));
        // Most-used first; the headline-only model appears with 0 calls.
        let models: Vec<(&str, u64)> = d
            .models
            .iter()
            .map(|m| (m.model.as_str(), m.calls))
            .collect();
        assert_eq!(models, vec![("A", 2), ("B", 1), ("C", 0)]);
    }

    #[test]
    fn runtime_model_observation_supersedes_configured_provenance() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "model-source", 1);
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        for (id, model, source, at) in [
            ("o1", "antigravity-pro", "configured_client", 1),
            ("o2", "antigravity-pro", "runtime_hook", 2),
            ("o3", "configured-only", "configured_client", 3),
        ] {
            rt.block_on(store.insert(
                "model_observation",
                &[
                    "observation_id",
                    "session_id",
                    "model",
                    "source",
                    "observed_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text("s"),
                    Value::text(model),
                    Value::text(source),
                    Value::Int(at),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        let detail = detail_of(root.path(), &path_of(&proj));
        assert_eq!(
            detail
                .models
                .iter()
                .find(|model| model.model == "antigravity-pro")
                .map(|model| model.source),
            Some(ModelProvenance::Runtime)
        );
        assert_eq!(
            detail
                .models
                .iter()
                .find(|model| model.model == "configured-only")
                .map(|model| model.source),
            Some(ModelProvenance::Configured)
        );
    }

    #[test]
    fn capture_summary_reflects_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "cap", 4); // 4 captured events
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_session(&store, &rt, "s1", "A");
        seed_checkpoint(&store, &rt, "k1", 100);
        seed_checkpoint(&store, &rt, "k2", 200);

        let d = detail_of(root.path(), &path_of(&proj));
        assert_eq!(d.capture.events, 4);
        assert_eq!(d.capture.sessions, 1);
        assert_eq!(d.capture.checkpoints, 2);
        assert_eq!(d.capture.last_checkpoint_at_ms, Some(200));
    }

    #[test]
    fn detail_is_scope_filtered() {
        // Two projects with independent projections: one's models never bleed into
        // the other's detail.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 1);
        let b = make_project(root.path(), "b", 1);
        let sa = open_proj(root.path(), &a);
        seed_model_call(&sa, &runtime(), "c1", "OnlyInA");

        let da = detail_of(root.path(), &path_of(&a));
        let db = detail_of(root.path(), &path_of(&b));
        assert!(da.models.iter().any(|m| m.model == "OnlyInA"));
        assert!(
            db.models.iter().all(|m| m.model != "OnlyInA"),
            "project b's detail must not reflect project a's models"
        );
    }

    /// The card says what the settings page says. A family whose git hooks
    /// were never installed is setup left half done, however healthy its
    /// projection reads; with the install on record the same projection —
    /// nothing captured in it yet — is a repository picking up.
    #[test]
    fn a_projection_with_no_recorded_install_is_setup_incomplete_on_the_card() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "fresh", 0);
        std::fs::remove_file(rift_shadow_hooks::binary_of_record_path(&family_dir_of(
            &proj,
        )))
        .unwrap();
        assert_eq!(
            read_project(root.path(), &proj.to_string_lossy()).status,
            ProjectStatus::SetupIncomplete
        );

        record_hooks_installed(&proj);
        assert_eq!(
            read_project(root.path(), &proj.to_string_lossy()).status,
            ProjectStatus::PickingUp
        );
    }

    /// A clock value for the health read's own tests.
    const NOW: i64 = 1_800_000_000_000;

    /// Wire Claude Code's project hook file to run `binary`, as `rift enable`
    /// writes it.
    fn wire_claude_hook(repo: &Path, binary: &Path) -> PathBuf {
        let file = repo.join(".claude/settings.local.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let body = serde_json::json!({ "hooks": { "SessionStart": [{ "hooks": [{
            "type": "command",
            "command": format!("\"{}\" hooks claude-code SessionStart", binary.display()),
        }]}]}});
        std::fs::write(&file, body.to_string()).unwrap();
        file
    }

    /// An executable file at `path` that is not the binary of record.
    fn other_binary(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.to_path_buf()
    }

    /// The sources for `repo`'s family with a temporary Codex home.
    fn sources_for<'a>(
        root: &Path,
        repo: &'a Path,
        members: &'a [FamilyMember],
    ) -> HealthSources<'a> {
        let family = project_family(repo).unwrap();
        HealthSources {
            repo,
            family_dir: family_dir(&family),
            projection_db: projection_db_for_family_under(root, &family),
            members,
            codex_home: Some(root.join("codex-home")),
        }
    }

    /// A detection environment over empty temporary directories.
    fn empty_machine(root: &Path) -> DetectionEnvironment {
        DetectionEnvironment {
            search_path: Some(root.join("bin").into_os_string()),
            home: Some(root.join("home")),
            app_roots: vec![root.join("apps")],
            codex_home: None,
        }
    }

    fn members_of(repo: &Path) -> Vec<FamilyMember> {
        crate::session::roster(&rift_git::discover(repo).unwrap()).members
    }

    #[test]
    fn an_error_severity_finding_turns_an_idle_project_to_an_integration_error() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        assert_eq!(status(root.path(), &path_of(&proj)), ProjectStatus::Idle);

        wire_claude_hook(&proj, &root.path().join("gone/rift"));
        assert_eq!(
            status(root.path(), &path_of(&proj)),
            ProjectStatus::Error(StatusError::Integration)
        );
    }

    #[test]
    fn an_error_severity_finding_turns_a_running_project_to_an_integration_error() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        live_session(&proj.join(".git"), "s1", &proj, now_ms());
        assert!(matches!(
            status(root.path(), &path_of(&proj)),
            ProjectStatus::Running { .. }
        ));

        wire_claude_hook(&proj, &root.path().join("gone/rift"));
        assert_eq!(
            status(root.path(), &path_of(&proj)),
            ProjectStatus::Error(StatusError::Integration)
        );
    }

    #[test]
    fn a_warning_only_project_stays_idle() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        wire_claude_hook(&proj, &other_binary(&root.path().join("other/rift")));

        let members = members_of(&proj);
        let sources = sources_for(root.path(), &proj, &members);
        let remedies = integration_remedies(&sources, &empty_machine(root.path()), NOW);
        let reasons: Vec<_> = remedies.iter().map(|remedy| remedy.reason).collect();
        assert_eq!(reasons, [health::Reason::HookBinaryDisagrees]);
        assert_eq!(status(root.path(), &path_of(&proj)), ProjectStatus::Idle);
    }

    #[test]
    fn the_remedies_name_the_failing_agent_with_the_command_lines_sentence() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        wire_claude_hook(&proj, &root.path().join("gone/rift"));

        let members = members_of(&proj);
        let sources = sources_for(root.path(), &proj, &members);
        let remedies = integration_remedies(&sources, &empty_machine(root.path()), NOW);
        assert_eq!(remedies.len(), 1, "{remedies:?}");
        assert_eq!(remedies[0].agent, "claude-code");
        assert_eq!(remedies[0].reason, health::Reason::HookBinaryMissing);
        assert_eq!(remedies[0].headline, "Claude Code: hook binary missing");
        assert!(integration_failing(&sources, NOW));
    }

    /// Every file under `dir`, with its bytes, in path order.
    fn tree_bytes(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    files.push((path, bytes));
                }
            }
        }
        files.sort();
        files
    }

    #[test]
    fn the_status_read_writes_nothing_the_health_read_reads() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        let hook = wire_claude_hook(&proj, &root.path().join("gone/rift"));
        live_session(&proj.join(".git"), "s1", &proj, NOW);
        let watched = [
            proj.join(".claude"),
            proj.join(".git/rift-sessions"),
            family_dir_of(&proj),
        ];
        let before: Vec<_> = watched.iter().map(|dir| tree_bytes(dir)).collect();
        assert!(before.iter().all(|files| !files.is_empty()));
        assert!(before[0].iter().any(|(path, _)| *path == hook));

        let read = read_project(root.path(), &path_of(&proj));
        assert_eq!(read.status, ProjectStatus::Error(StatusError::Integration));
        let _ = project_remedies(root.path(), &proj);
        let after: Vec<_> = watched.iter().map(|dir| tree_bytes(dir)).collect();
        assert_eq!(before, after);
    }

    /// The status read for one project, timed: the whole read and the
    /// health read inside it, so the poll's cost of the finding is a number.
    #[test]
    #[ignore = "measurement instrument: prints the status read's cost"]
    fn measure_the_status_read_and_its_health_share() {
        const ROUNDS: u32 = 200;
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 200);
        wire_claude_hook(&proj, &root.path().join("gone/rift"));
        for index in 0..20 {
            live_session(&proj.join(".git"), &format!("s{index}"), &proj, now_ms());
        }
        let path = path_of(&proj);
        let started = std::time::Instant::now();
        for _ in 0..ROUNDS {
            let _ = read_project(root.path(), &path);
        }
        let whole = started.elapsed() / ROUNDS;
        let members = members_of(&proj);
        let sources = sources_for(root.path(), &proj, &members);
        let started = std::time::Instant::now();
        for _ in 0..ROUNDS {
            let _ = integration_failing(&sources, NOW);
        }
        let health_share = started.elapsed() / ROUNDS;
        println!("status read {whole:?} per project, health read {health_share:?} of it");
    }
}
