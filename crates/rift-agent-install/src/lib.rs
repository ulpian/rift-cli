//! Project-scoped native agent installation for Rift Total Recall.
//!
//! This crate owns configuration I/O and diagnostics. Agent adapters remain
//! stateless translators with no filesystem authority.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod detect;
pub mod native;

/// The first-class clients configured by `rift recall init`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientFamily {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI/app.
    Codex,
    /// The legacy Gemini CLI hook/config surface.
    Gemini,
    /// Google Antigravity IDE/CLI.
    Antigravity,
}

impl ClientFamily {
    /// All supported clients, in stable display/install order.
    pub const ALL: [Self; 4] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
    ];

    /// The family with this registry token, if it is one of the four.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|family| family.token() == token)
    }

    /// Rift's stable CLI/ledger token for this client.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
        }
    }

    /// Human-facing client name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
        }
    }

    fn hook_path(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/settings.local.json",
            Self::Codex => ".codex/hooks.json",
            Self::Gemini => ".gemini/settings.json",
            Self::Antigravity => ".agents/hooks.json",
        }
    }
}

/// Whether an executable is present and capable enough for Rift's integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionState {
    /// No install evidence was found (see [`detect::installations`]).
    NotDetected,
    /// The client is installed and meets the known integration floor.
    Supported,
    /// The executable was found but is too old or otherwise incompatible.
    Unsupported,
}

/// The state of one native configuration surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WiringState {
    /// The expected Rift entry is absent.
    Missing,
    /// The expected Rift entry is present.
    Configured,
    /// A reserved Rift entry exists with unowned/different content.
    Conflict,
    /// The native configuration cannot be parsed.
    Invalid,
    /// The files are correct but the client still requires trust/restart/auth.
    ActionRequired,
    /// Rift's hooks are written and the client is skipping at least one of
    /// them: Codex, for a hook changed after, or never, trusted.
    Error,
}

/// Whether Rift's own stdio handshake probe has verified the generated server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerProbeState {
    /// No probe result is available.
    Unverified,
    /// A probe completed successfully.
    Verified,
    /// The last probe failed.
    Failed,
}

/// Truthful live-client connection state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// No real client handshake has ever been observed.
    NeverSeen,
    /// A fresh real-client lease exists.
    Connected,
    /// A historical handshake exists but no fresh lease remains.
    Disconnected,
}

/// Provenance of a model identifier in integration diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvenance {
    /// Observed in a native hook/model call.
    Runtime,
    /// Read from a concrete supported client configuration value.
    Configured,
}

/// Truthful model diagnostic for one client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStatus {
    /// Concrete model identifier, if known.
    pub identifier: Option<String>,
    /// Runtime/configured provenance for the identifier.
    pub provenance: Option<ModelProvenance>,
    /// Explicit reason when no model can be shown.
    pub unavailable_reason: Option<String>,
}

/// Read-only status for one client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStatus {
    /// Client family.
    pub client: ClientFamily,
    /// Detection/capability state, from the same evidence the native
    /// diagnostic reads ([`detect::installations`]).
    pub detection: DetectionState,
    /// The first runnable binary among `installations`, if any provides one.
    pub executable: Option<PathBuf>,
    /// Best-effort version output from running `executable`, when probed.
    pub version: Option<String>,
    /// Every install found, in [`detect`]'s reporting order.
    pub installations: Vec<detect::NativeInstallation>,
    /// Native capture hook configuration state. Retiring the server transport
    /// made hooks the whole of it: Rift writes and inspects nothing else in a client's
    /// settings, and the server-entry half this used to be paired with is
    /// retired.
    pub hooks: WiringState,
    /// Rift server probe state. Vestigial since the stdio transport was
    /// retired (nothing verifies a handshake anymore); kept for its own read path
    /// (`record_probe`/`read_probe_state`) rather than removed unasked.
    pub server_probe: ServerProbeState,
    /// Real client connection state. Vestigial for the same reason: the live-lease files `read_connection_lease` reads are no
    /// longer written by anything, so this now permanently reads `NeverSeen`.
    pub connection: ConnectionState,
    /// Last connection/lease observation, if known.
    pub last_seen_at_ms: Option<i64>,
    /// Most recent durable hook receipt for this client.
    pub last_hook_at_ms: Option<i64>,
    /// Most recent durable per-client tool call, read from the historical
    /// append-only call/connection relations. Nothing writes those relations
    /// now — the live `serve_call_event` axis is per channel, not per client,
    /// so a serve carries no client to attribute to — leaving this a
    /// read-compat fact over history rather than a live one.
    pub last_call_at_ms: Option<i64>,
    /// Latest truthful model identifier and provenance, or its unavailable reason.
    pub model: ModelStatus,
    /// One concise, actionable explanation when degraded.
    pub action: Option<String>,
    /// Codex's trust in each hook Rift wrote: present for Codex when its hook
    /// file holds a Rift hook, `None` for every other client.
    pub codex_trust: Option<native::CodexTrust>,
}

/// Installation/configuration failure. Never carries config or transcript bytes.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// A native file was malformed or had a conflicting reserved entry.
    #[error("{path}: {reason}")]
    Config {
        /// File involved.
        path: PathBuf,
        /// Neutral reason.
        reason: String,
    },
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// Diagnose native configuration, optional executables, and connection leases.
/// `probe_binaries=false` is appropriate for frequently refreshed UI reads.
#[must_use]
pub fn diagnose(repo: &Path, probe_binaries: bool) -> Vec<ClientStatus> {
    ClientFamily::ALL
        .into_iter()
        .map(|client| diagnose_client(repo, client, probe_binaries))
        .collect()
}

/// Persist Rift's own server-probe result. This proves only that the generated
/// stdio server is servable; it never marks a real client as connected.
///
/// # Errors
/// Returns [`InstallError`] if the probe marker cannot be written.
pub fn record_probe(repo: &Path, verified: bool) -> Result<(), InstallError> {
    let path = repo.join(".rift/agents/recall-probe.json");
    atomic_write(
        &path,
        &pretty_json(&serde_json::json!({
            "schema_version": 1,
            "verified": verified,
            "observed_at_ms": unix_ms(),
        })),
    )
}

fn diagnose_client(repo: &Path, client: ClientFamily, probe_binaries: bool) -> ClientStatus {
    diagnose_client_with_environment(
        repo,
        client,
        probe_binaries,
        &detect::DetectionEnvironment::from_process(),
    )
}

fn diagnose_client_with_environment(
    repo: &Path,
    client: ClientFamily,
    probe_binaries: bool,
    env: &detect::DetectionEnvironment,
) -> ClientStatus {
    // The same evidence the native diagnostic reads, so doctor and
    // `rift recall status` cannot disagree with `init` about what is installed.
    let installations = detect::installations(client.token(), env);
    let executable = detect::first_executable(&installations);
    let version = probe_binaries
        .then(|| executable.as_ref().and_then(|p| command_version(p)))
        .flatten();
    let detection = if installations.is_empty() {
        DetectionState::NotDetected
    } else if client == ClientFamily::Codex
        && version
            .as_deref()
            .and_then(parse_codex_minor)
            .is_some_and(|minor| minor < 100)
    {
        DetectionState::Unsupported
    } else {
        DetectionState::Supported
    };
    let codex_trust = (client == ClientFamily::Codex)
        .then(|| native::codex_trust(repo, env.codex_home.as_deref()))
        .flatten();
    let remedy = codex_trust.as_ref().and_then(native::CodexTrust::remedy);
    let hooks = match (inspect_hooks(repo, client), &remedy) {
        (WiringState::Configured, Some(_)) => WiringState::Error,
        (hooks, _) => hooks,
    };
    let (connection, last_seen_at_ms) = read_connection_lease(repo, client);
    let (last_hook_at_ms, model) = read_capture_facts(repo, client);
    let last_call_at_ms = read_last_call(repo, client);
    let action = remedy
        .or_else(|| readiness_action(client, detection, hooks, connection, version.as_deref()));
    ClientStatus {
        client,
        detection,
        executable,
        version,
        installations,
        hooks,
        server_probe: read_probe_state(repo),
        connection,
        last_seen_at_ms,
        last_hook_at_ms,
        last_call_at_ms,
        model,
        action,
        codex_trust,
    }
}

fn readiness_action(
    client: ClientFamily,
    detection: DetectionState,
    hooks: WiringState,
    connection: ConnectionState,
    version: Option<&str>,
) -> Option<String> {
    match (detection, hooks, connection) {
        (DetectionState::Unsupported, _, _) => Some(format!(
            "upgrade {}; detected {}",
            client.display_name(),
            version.unwrap_or("unknown version")
        )),
        (_, WiringState::Invalid | WiringState::Conflict, _) => Some(format!(
            "repair {} hook configuration",
            client.display_name()
        )),
        (_, WiringState::Missing, _) => Some("run `rift recall init`".to_owned()),
        (DetectionState::Supported, WiringState::Configured, ConnectionState::NeverSeen) => {
            Some(format!(
                "restart/trust/authenticate {} and inspect its native hooks status",
                client.display_name()
            ))
        }
        _ => None,
    }
}

fn hook_fragment(client: ClientFamily, rift_bin: &Path) -> serde_json::Value {
    if client == ClientFamily::Antigravity {
        let mut bundle = serde_json::Map::new();
        bundle.insert("enabled".to_owned(), serde_json::Value::Bool(true));
        for event in &standard_events(client) {
            let handler = hook_handler(client, event, rift_bin);
            let value = if matches!(*event, "PreToolUse" | "PostToolUse") {
                serde_json::json!([{ "matcher": "*", "hooks": [handler] }])
            } else {
                serde_json::json!([handler])
            };
            bundle.insert((*event).to_owned(), value);
        }
        return serde_json::Value::Object(bundle);
    }

    let mut hooks = serde_json::Map::new();
    for event in &standard_events(client) {
        hooks.insert(
            (*event).to_owned(),
            standard_event_group(client, event, rift_bin),
        );
    }
    serde_json::Value::Object(hooks)
}

/// The exact group array Rift registers under one hook event for a standard
/// (non-Antigravity) client: a single `{ ("matcher": "*")?, "hooks": [handler] }`
/// entry, matcher present iff the event is a tool event.
///
/// This is the single emitter both directions share: [`hook_fragment`] writes
/// with it, and [`recognizable_rift_hook_fragment`] recognizes by re-emitting
/// with it — so the writer and the recognizer cannot drift.
fn standard_event_group(client: ClientFamily, event: &str, rift_bin: &Path) -> serde_json::Value {
    let mut group = serde_json::Map::new();
    if is_tool_event(event) {
        group.insert(
            "matcher".to_owned(),
            serde_json::Value::String("*".to_owned()),
        );
    }
    group.insert(
        "hooks".to_owned(),
        serde_json::json!([hook_handler(client, event, rift_bin)]),
    );
    serde_json::json!([group])
}

/// The absolute binary quoted by the first Rift hook command anywhere in
/// `value` — for **every** family, Antigravity's reserved bundle included.
///
/// This is the ownership fact a verify-only caller needs (which `rift` does
/// this installed fragment invoke?) without adopting or rewriting anything,
/// and it is the first step [`recognizable_rift_hook_fragment`] takes before
/// it re-emits, so the two cannot disagree about what a command names.
///
/// A relative or unquoted command yields `None`: Rift writes an absolute path
/// in quotes, always, and a command that does not have one was not written by
/// this emitter.
fn rift_hook_binary(value: &serde_json::Value, client: ClientFamily) -> Option<PathBuf> {
    let command = first_rift_hook_command(value, client)?;
    if !command.starts_with('"') {
        return None;
    }
    let marker = format!("\" hooks {} ", client.token());
    let binary = PathBuf::from(command.get(1..command.find(&marker)?)?);
    absolute_on_any_platform(&binary).then_some(binary)
}

/// The one sentence every surface prints for install evidence: `absent`, or
/// `present via <source> [<version>]` per install, joined by ` + `. Both
/// diagnostics ([`ClientStatus`] and [`native::NativeIntegrationStatus`]) and
/// Canopy's row spell it this way, so a user surprised by an auto-wire reads
/// the same reason everywhere.
#[must_use]
pub fn describe_installations(installations: &[detect::NativeInstallation]) -> String {
    if installations.is_empty() {
        return "absent".to_owned();
    }
    format!(
        "present via {}",
        installations
            .iter()
            .map(detect::NativeInstallation::describe)
            .collect::<Vec<_>>()
            .join(" + ")
    )
}

/// Whether `path` is absolute on the platform that wrote it — POSIX
/// (`/Users/…`) or Windows (`C:\…`, `\\server\…`) — rather than only on the
/// platform reading it.
///
/// `Path::is_absolute` answers for the running platform: `/Users/x/rift` is
/// not absolute on Windows, `C:\rift` is not absolute on macOS. Every
/// ownership proof here requires an absolute binary, so a hook file or
/// descriptor written on one machine failed the proof on the other and read
/// as a foreign **conflict** the moment a repository was cloned across
/// platforms — the tracked Antigravity bundle in particular, which travels
/// with the clone by design. Ownership is about what Rift wrote, not where it
/// is being read; a bundle that names another machine's rift is Rift's own
/// output pointing elsewhere, which is the refreshable class, never the
/// conflict class.
#[must_use]
pub fn absolute_on_any_platform(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    // POSIX: a leading slash. UNC: a leading `\\`. Drive: `C:\` or `C:/`.
    matches!(bytes, [b'/', ..] | [b'\\', b'\\', ..])
        || matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

/// Recognize a standard-client hook fragment as Rift's own output from ANY
/// verb table, and return the binary it was written for.
///
/// The predicate is re-emission over the candidate's OWN key set: extract the
/// absolute binary from the first Rift hook command, then require every
/// `(event, groups)` pair to equal [`standard_event_group`] for that event and
/// binary — one consistent binary throughout, group key equal to the verb in
/// the command, the exact handler shape, matcher iff tool event. That makes
/// the recognizer version-proof in the growth direction (a fragment written
/// from a superseded, smaller verb table still recognizes) while a single
/// wrong byte — an edited timeout, a second handler, a verb that does not
/// match its key, a relative or inconsistent binary — still refuses, so a
/// merely similar user file is never adopted.
///
/// Antigravity is deliberately `None`: its reserved `rift-recall` bundle has a
/// different shape and its own ownership arm in [`merge_antigravity_hooks`].
fn recognizable_rift_hook_fragment(
    fragment: &serde_json::Value,
    client: ClientFamily,
) -> Option<PathBuf> {
    if client == ClientFamily::Antigravity {
        return None;
    }
    let binary = rift_hook_binary(fragment, client)?;
    let groups = fragment.as_object()?;
    groups
        .iter()
        .all(|(event, value)| value == &standard_event_group(client, event, &binary))
        .then_some(binary)
}

/// The first string anywhere in `value` that quotes a binary and invokes
/// `hooks <token>` — the command [`recognizable_rift_hook_fragment`] extracts
/// its candidate binary from.
fn first_rift_hook_command(value: &serde_json::Value, client: ClientFamily) -> Option<&str> {
    let marker = format!("\" hooks {} ", client.token());
    first_string_containing(value, &marker)
}

fn first_string_containing<'a>(value: &'a serde_json::Value, marker: &str) -> Option<&'a str> {
    match value {
        serde_json::Value::String(text) if text.contains(marker) => Some(text),
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|v| first_string_containing(v, marker)),
        serde_json::Value::Object(items) => items
            .values()
            .find_map(|v| first_string_containing(v, marker)),
        _ => None,
    }
}

fn hook_handler(client: ClientFamily, event: &str, rift_bin: &Path) -> serde_json::Value {
    let mut handler = serde_json::json!({
        "type": "command",
        "command": format!("\"{}\" hooks {} {event}", rift_bin.display(), client.token()),
        "timeout": 10,
    });
    if let (Some(limit), Some(map)) = (
        declared_context_limit(client, event),
        handler.as_object_mut(),
    ) {
        map.insert("additionalContextLimit".to_owned(), limit.into());
    }
    handler
}

/// The `additionalContext` ceiling `client`'s hook configuration declares for
/// `event`, if that family bounds the channel by a key written here — **a
/// projection of the adapter's own `declared_context_limit`, never a second
/// number kept in this file.**
///
/// The reason it may not be a second number is the same reason
/// [`standard_events`] is not a second verb table, and it is sharper here: the
/// client honours whatever this writes, and the hook that answers on the
/// channel fits its text to what it believes was written. Two numbers that
/// drifted would not fail loudly — the client would keep a head and a tail of
/// a frame, spill the rest to a file, and the model would read a preview.
fn declared_context_limit(client: ClientFamily, event: &str) -> Option<u32> {
    rift_shadow_hooks::AdapterRegistry::with_builtin()
        .get(&rift_core::AgentId::new(client.token()))
        .and_then(|adapter| adapter.declared_context_limit(event))
}

/// The hook events Rift registers for `client` — **a projection of the
/// adapter's own [`AgentAdapter::verbs`], never a second table** (an owner
/// decision of 2026-08-24).
///
/// **This function used to be the second table, and the two disagreed on
/// disk.** It listed eight events for Claude Code;
/// `ClaudeCodeAdapter::verbs()` declares nine. The consequence was live in this
/// repository's own checkout: `.claude/settings.local.json` carried eight hook
/// keys while `.rift/agents/claude-code.json` — written from `verbs()` in the
/// same `rift enable` call — declared nine, so Rift advertised a
/// `SubagentStart` capture it had never registered a hook for. Nothing reddened,
/// because nothing compared them.
///
/// One table now decides both, and
/// [`the_registered_hook_events_are_exactly_the_adapters_declared_verbs`] reads
/// the fragment the installer actually writes rather than this function, so the
/// agreement is asserted at the door and not at the definition.
///
/// A family with no registered adapter yields no events rather than panicking:
/// `enable` refuses that case earlier and with a better message
/// (`native::enable`, "no built-in adapter exists for this agent").
fn standard_events(client: ClientFamily) -> Vec<&'static str> {
    rift_shadow_hooks::AdapterRegistry::with_builtin()
        .get(&rift_core::AgentId::new(client.token()))
        .map(|adapter| adapter.verbs().to_vec())
        .unwrap_or_default()
}

fn is_tool_event(event: &str) -> bool {
    matches!(
        event,
        "PreToolUse" | "PostToolUse" | "BeforeTool" | "AfterTool"
    )
}

fn merge_standard_hooks(
    path: &Path,
    client: ClientFamily,
    expected: &serde_json::Value,
    previous_fingerprint: Option<&str>,
    adopt: bool,
) -> Result<Vec<u8>, InstallError> {
    let mut root = read_json_object(path)?;
    insert_standard_hooks(
        &mut root,
        path,
        client,
        expected,
        previous_fingerprint,
        adopt,
    )?;
    Ok(pretty_json(&serde_json::Value::Object(root)))
}

fn insert_standard_hooks(
    root: &mut serde_json::Map<String, serde_json::Value>,
    path: &Path,
    client: ClientFamily,
    expected: &serde_json::Value,
    previous_fingerprint: Option<&str>,
    adopt: bool,
) -> Result<(), InstallError> {
    let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return config_err(path, "`hooks` is not an object");
    };
    let current_fragment = hook_subset(hooks, client);
    let has_rift_hooks = fragment_contains_rift_command(&current_fragment, client);
    let changed_after_install = previous_fingerprint.is_some()
        && has_rift_hooks
        && !fingerprint_matches(&current_fragment, previous_fingerprint);
    // A fragment that no longer matches the fingerprint but is still exactly
    // Rift's own emission — for any binary, from any verb table — was rewritten
    // by Rift's OTHER installer (`native::enable` refreshes after a table
    // move), not edited by the user. The two installers keep separate
    // manifests, so without this arm each would call the other's refresh
    // "user-modified" and refuse. A fragment a user actually touched fails the
    // shape on the first wrong byte and is refused exactly as before.
    if changed_after_install
        && !adopt
        && recognizable_rift_hook_fragment(&current_fragment, client).is_none()
    {
        return config_err(
            path,
            "installed Rift hooks changed; refusing to replace a user's edits",
        );
    }

    let expected = expected.as_object().cloned().unwrap_or_default();
    for (event, groups) in expected {
        let target = hooks
            .entry(event.clone())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let Some(target) = target.as_array_mut() else {
            return config_err(path, "hook event is not an array");
        };
        // A recognizable Rift command without an ownership manifest is a
        // legacy installation. Upgrade it automatically; this is the migration
        // path used by repositories that had Recall before multi-client
        // bootstrap existed. A manifest-backed fragment is protected by the
        // fingerprint check above, so user edits are still never overwritten.
        target.retain(|group| !group_contains_rift_command(group, client));
        if let Some(groups) = groups.as_array() {
            // Codex 0.153.1 gives the first sibling input rewrite
            // deterministic precedence while every sibling sees the original
            // input. Put Rift first for the one event whose bounded read
            // replacement uses that channel; preserve unrelated order after it.
            if client == ClientFamily::Codex && event == "PreToolUse" {
                target.splice(0..0, groups.iter().cloned());
            } else {
                target.extend(groups.iter().cloned());
            }
        }
    }
    Ok(())
}

fn merge_antigravity_hooks(
    path: &Path,
    expected: &serde_json::Value,
    previous_fingerprint: Option<&str>,
    adopt: bool,
) -> Result<Vec<u8>, InstallError> {
    let mut root = read_json_object(path)?;
    if let Some(existing) = root.get("rift-recall") {
        let changed_after_install =
            previous_fingerprint.is_some() && !fingerprint_matches(existing, previous_fingerprint);
        let unrecognized_legacy = previous_fingerprint.is_none()
            && !fragment_contains_rift_command(existing, ClientFamily::Antigravity);
        if existing != expected && (changed_after_install || unrecognized_legacy) && !adopt {
            return config_err(
                path,
                "reserved `rift-recall` hook bundle is not safely Rift-owned; refusing to replace it",
            );
        }
    }
    root.insert("rift-recall".to_owned(), expected.clone());
    Ok(pretty_json(&serde_json::Value::Object(root)))
}

fn group_contains_rift_command(value: &serde_json::Value, client: ClientFamily) -> bool {
    fragment_contains_rift_command(value, client)
}

fn fragment_contains_rift_command(value: &serde_json::Value, client: ClientFamily) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains(&format!(" hooks {} ", client.token())),
        serde_json::Value::Array(a) => a.iter().any(|v| fragment_contains_rift_command(v, client)),
        serde_json::Value::Object(o) => o
            .values()
            .any(|v| fragment_contains_rift_command(v, client)),
        _ => false,
    }
}

fn inspect_hooks(repo: &Path, client: ClientFamily) -> WiringState {
    let path = repo.join(client.hook_path());
    if !path.exists() {
        return WiringState::Missing;
    }
    let Ok(root) = read_json_object(&path) else {
        return WiringState::Invalid;
    };
    let fragment = if client == ClientFamily::Antigravity {
        root.get("rift-recall")
    } else {
        root.get("hooks")
    };
    match fragment {
        Some(v) if fragment_contains_rift_command(v, client) => WiringState::Configured,
        Some(_) | None => WiringState::Missing,
    }
}

fn remove_standard_hooks(
    path: &Path,
    client: ClientFamily,
    expected_fingerprint: Option<&str>,
) -> Result<Option<Vec<u8>>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut root = read_json_object(path)?;
    let Some(hooks) = root
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(None);
    };
    let mut removed = false;
    let current_fragment = hook_subset(hooks, client);
    // Same exemption as `insert_standard_hooks`: a fragment the OTHER
    // installer refreshed is still Rift's own emission by shape, and removal
    // of Rift's own groups needs no adoption. A user-edited fragment fails
    // the shape and is refused exactly as before.
    if fragment_contains_rift_command(&current_fragment, client)
        && !fingerprint_matches(&current_fragment, expected_fingerprint)
        && recognizable_rift_hook_fragment(&current_fragment, client).is_none()
    {
        return config_err(path, "Rift hooks changed; refusing to remove them");
    }
    for event in &standard_events(client) {
        if let Some(groups) = hooks
            .get_mut(*event)
            .and_then(serde_json::Value::as_array_mut)
        {
            let before = groups.len();
            groups.retain(|g| !group_contains_rift_command(g, client));
            removed |= before != groups.len();
        }
    }
    hooks.retain(|_, v| !v.as_array().is_some_and(Vec::is_empty));
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(removed.then(|| pretty_json(&serde_json::Value::Object(root))))
}

fn remove_antigravity_hooks(
    path: &Path,
    expected_fingerprint: Option<&str>,
) -> Result<Option<Vec<u8>>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut root = read_json_object(path)?;
    let Some(current) = root.get("rift-recall") else {
        return Ok(None);
    };
    if !fingerprint_matches(current, expected_fingerprint) {
        return config_err(path, "Rift hook bundle changed; refusing to remove it");
    }
    root.remove("rift-recall");
    Ok(Some(pretty_json(&serde_json::Value::Object(root))))
}

fn hook_subset(
    hooks: &serde_json::Map<String, serde_json::Value>,
    client: ClientFamily,
) -> serde_json::Value {
    let mut subset = serde_json::Map::new();
    for event in &standard_events(client) {
        let groups = hooks
            .get(*event)
            .and_then(serde_json::Value::as_array)
            .map(|groups| {
                groups
                    .iter()
                    .filter(|g| group_contains_rift_command(g, client))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !groups.is_empty() {
            subset.insert((*event).to_owned(), serde_json::Value::Array(groups));
        }
    }
    serde_json::Value::Object(subset)
}

fn fingerprint_matches(value: &serde_json::Value, expected: Option<&str>) -> bool {
    expected.is_some_and(|e| fingerprint_json(value) == e)
}

fn read_json_object(
    path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, InstallError> {
    let text = read_text(path)?;
    if text.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .ok_or_else(|| InstallError::Config {
            path: path.to_path_buf(),
            reason: "invalid JSON object".to_owned(),
        })
}

fn read_text(path: &Path) -> Result<String, InstallError> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn pretty_json(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec());
    bytes.push(b'\n');
    bytes
}

fn fingerprint_json(value: &serde_json::Value) -> String {
    blake3::hash(&serde_json::to_vec(value).unwrap_or_default())
        .to_hex()
        .to_string()
}

fn config_err<T>(path: &Path, reason: &str) -> Result<T, InstallError> {
    Err(InstallError::Config {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".rift-tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(|source| InstallError::Io {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path).map_err(|source| InstallError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn command_version(binary: &Path) -> Option<String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if started.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    let _ = child.stdout.take()?.read_to_string(&mut stdout);
    let _ = child.stderr.take()?.read_to_string(&mut stderr);
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

fn parse_codex_minor(version: &str) -> Option<u64> {
    let token = version
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.'))?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    (major == 0).then_some(minor)
}

fn read_capture_facts(repo: &Path, client: ClientFamily) -> (Option<i64>, ModelStatus) {
    use rift_store::{CanonicalStore as _, Value};

    let unavailable = || ModelStatus {
        identifier: None,
        provenance: None,
        unavailable_reason: Some(if client == ClientFamily::Antigravity {
            "no runtime model or concrete configured model".to_owned()
        } else {
            "no runtime model hook observed".to_owned()
        }),
    };
    let Ok(db) = rift_store::capture_projection_db_for_repo(repo) else {
        return (
            None,
            configured_model_fallback(client).unwrap_or_else(unavailable),
        );
    };
    if !db.exists() {
        return (
            None,
            configured_model_fallback(client).unwrap_or_else(unavailable),
        );
    }
    let Ok(store) = rift_store::SqliteStore::open_read_only(&db) else {
        return (
            None,
            configured_model_fallback(client).unwrap_or_else(unavailable),
        );
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
        return (
            None,
            configured_model_fallback(client).unwrap_or_else(unavailable),
        );
    };
    let agent = client.token();
    let last_hook_at_ms = runtime
        .block_on(store.query_opt_i64(
            "SELECT MAX(received_at) FROM agent_hook_event WHERE agent = ?",
            &[Value::text(agent)],
        ))
        .ok()
        .flatten();
    let observations = runtime
        .block_on(store.query(
            "SELECT o.model, o.source \
             FROM model_observation o JOIN session s ON s.session_id = o.session_id \
             WHERE s.agent = ? AND TRIM(o.model) <> '' \
             ORDER BY CASE o.source WHEN 'runtime_hook' THEN 1 ELSE 0 END DESC, \
                      o.observed_at DESC LIMIT 1",
            &[Value::text(agent)],
        ))
        .unwrap_or_default();
    let observed = observations.first().and_then(|row| {
        let model = row.get_text(0).ok()?;
        concrete_model(&model)?;
        let source = row.get_text(1).ok()?;
        Some(ModelStatus {
            identifier: Some(bounded_text(&model)),
            provenance: Some(if source == "configured_client" {
                ModelProvenance::Configured
            } else {
                ModelProvenance::Runtime
            }),
            unavailable_reason: None,
        })
    });
    let model = observed
        .or_else(|| read_call_model(&runtime, &store, agent))
        .or_else(|| configured_model_fallback(client))
        .unwrap_or_else(unavailable);
    (last_hook_at_ms, model)
}

fn read_call_model(
    runtime: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    agent: &str,
) -> Option<ModelStatus> {
    use rift_store::{CanonicalStore as _, Value};

    let rows = runtime
        .block_on(store.query(
            "SELECT c.model FROM model_call c \
             JOIN turn t ON t.turn_id = c.turn_id \
             JOIN session s ON s.session_id = t.session_id \
             WHERE s.agent = ? AND TRIM(c.model) <> '' \
             ORDER BY c.started_at DESC LIMIT 1",
            &[Value::text(agent)],
        ))
        .ok()?;
    let model = rows.first()?.get_text(0).ok()?;
    concrete_model(&model)?;
    Some(ModelStatus {
        identifier: Some(bounded_text(&model)),
        provenance: Some(ModelProvenance::Runtime),
        unavailable_reason: None,
    })
}

fn configured_model_fallback(client: ClientFamily) -> Option<ModelStatus> {
    if client != ClientFamily::Antigravity {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        home.join(".gemini/antigravity-cli/settings.json"),
        home.join(".gemini/antigravity/settings.json"),
    ]
    .into_iter()
    .find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let model = value.get("model")?.as_str()?;
        concrete_model(model)?;
        Some(ModelStatus {
            identifier: Some(bounded_text(model)),
            provenance: Some(ModelProvenance::Configured),
            unavailable_reason: None,
        })
    })
}

fn concrete_model(model: &str) -> Option<()> {
    let model = model.trim();
    (!model.is_empty()
        && !model.eq_ignore_ascii_case("auto")
        && !model.eq_ignore_ascii_case("default"))
    .then_some(())
}

fn bounded_text(value: &str) -> String {
    value.chars().take(128).collect()
}

fn read_last_call(repo: &Path, client: ClientFamily) -> Option<i64> {
    use rift_store::{CanonicalStore as _, Value};

    let db = rift_store::recall_db_for_repo(repo).ok()?;
    if !db.exists() {
        return None;
    }
    let store = rift_store::SqliteStore::open_read_only(&db).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread().build().ok()?;
    runtime
        // Historical append-only relations, named literally because that is
        // what the rows are stored under. Nothing writes them now.
        .block_on(store.query_opt_i64(
            "SELECT MAX(c.recorded_at) FROM memd_call_event c \
             JOIN mcp_connection_event e ON e.thread_id = c.thread_id \
             WHERE e.client = ?",
            &[Value::text(client.token())],
        ))
        .ok()
        .flatten()
}

fn read_probe_state(repo: &Path) -> ServerProbeState {
    let path = repo.join(".rift/agents/recall-probe.json");
    let Ok(bytes) = std::fs::read(path) else {
        return ServerProbeState::Unverified;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ServerProbeState::Unverified;
    };
    if value.get("verified").and_then(serde_json::Value::as_bool) == Some(true) {
        ServerProbeState::Verified
    } else {
        ServerProbeState::Failed
    }
}

fn read_connection_lease(repo: &Path, client: ClientFamily) -> (ConnectionState, Option<i64>) {
    let Ok(state_dir) = rift_store::connection_state_dir_for_repo(repo) else {
        return (ConnectionState::NeverSeen, None);
    };
    read_connection_lease_from(&state_dir, client)
}

fn read_connection_lease_from(
    state_dir: &Path,
    client: ClientFamily,
) -> (ConnectionState, Option<i64>) {
    let lease_dir = state_dir.join("live").join(client.token());
    let history = state_dir
        .join("last-seen")
        .join(format!("{}.json", client.token()));
    let lease_at = std::fs::read_dir(&lease_dir).ok().and_then(|entries| {
        entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| read_observed_at(&path))
            .max()
    });
    let history_at = read_observed_at(&history);
    let now = unix_ms();
    if lease_at.is_some_and(|at| now.saturating_sub(at) <= 15_000) {
        return (ConnectionState::Connected, lease_at);
    }
    if let Some(at) = history_at.or(lease_at) {
        return (ConnectionState::Disconnected, Some(at));
    }
    (ConnectionState::NeverSeen, None)
}

fn read_observed_at(path: &Path) -> Option<i64> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("observed_at_ms")?
        .as_i64()
}

fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The installer registers exactly the verbs the adapter declares, per
    /// family** (an owner decision of 2026-08-24).
    ///
    /// This reads the fragment [`hook_fragment`] actually produces rather than
    /// [`standard_events`], and the distinction is the whole point: with
    /// `standard_events` now a projection of `verbs()`, comparing the two
    /// would be a tautology that passes whatever the writer does with the list.
    /// The fragment is what lands in `.claude/settings.local.json` and
    /// `.codex/hooks.json`, so asserting *its* keys is asserting the thing a
    /// reader of those files would check.
    ///
    /// **The defect this closes was live on disk.** Before that decision the
    /// installer held a second, hand-maintained table. It listed eight events
    /// for Claude Code against the adapter's nine, and this repository's own
    /// checkout showed the split exactly: eight hook keys in
    /// `.claude/settings.local.json`, nine verbs in
    /// `.rift/agents/claude-code.json`, both written by the same `rift enable`.
    /// Rift advertised a `SubagentStart` capture it had never wired. Nothing
    /// reddened because nothing compared them — which is what this test is.
    #[test]
    fn the_registered_hook_events_are_exactly_the_adapters_declared_verbs() {
        let registry = rift_shadow_hooks::AdapterRegistry::with_builtin();
        for client in ClientFamily::ALL {
            let adapter = registry
                .get(&rift_core::AgentId::new(client.token()))
                .unwrap_or_else(|| panic!("{} has no registered adapter", client.token()));
            let fragment = hook_fragment(client, Path::new("/opt/rift/bin/rift"));
            let mut written: Vec<String> = fragment
                .as_object()
                .expect("a hook fragment is a JSON object")
                .keys()
                // Antigravity's fragment carries an `enabled` flag beside its
                // event keys; it is a switch, not a verb.
                .filter(|key| key.as_str() != "enabled")
                .cloned()
                .collect();
            written.sort();
            let mut declared: Vec<String> =
                adapter.verbs().iter().map(|v| (*v).to_owned()).collect();
            declared.sort();
            assert_eq!(
                written,
                declared,
                "{}: the hooks Rift registers and the verbs its adapter declares \
                 must be one table",
                client.token()
            );
        }
    }

    #[test]
    fn fresh_and_stale_connection_leases_are_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        // The on-disk segment is frozen read-compat; see
        // `rift_store::connection_state_dir_under`.
        let state = tmp.path().join("mcp");
        let live = state.join("live/codex");
        let history = state.join("last-seen");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&history).unwrap();
        let now = unix_ms();
        std::fs::write(
            live.join("thread.json"),
            serde_json::to_vec(&serde_json::json!({"observed_at_ms": now})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_connection_lease_from(&state, ClientFamily::Codex).0,
            ConnectionState::Connected
        );
        std::fs::write(
            live.join("thread.json"),
            serde_json::to_vec(&serde_json::json!({"observed_at_ms": now - 16_000})).unwrap(),
        )
        .unwrap();
        std::fs::write(
            history.join("codex.json"),
            serde_json::to_vec(&serde_json::json!({"observed_at_ms": now - 16_000})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_connection_lease_from(&state, ClientFamily::Codex).0,
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn automatic_model_names_are_not_concrete() {
        assert!(concrete_model("").is_none());
        assert!(concrete_model("auto").is_none());
        assert!(concrete_model("DEFAULT").is_none());
        assert!(concrete_model("gemini-2.5-pro").is_some());
    }

    #[test]
    fn antigravity_shape_is_named_and_pretool_is_gated() {
        let bundle = hook_fragment(ClientFamily::Antigravity, Path::new("/opt/rift/bin/rift"));
        assert!(bundle["PreToolUse"].is_array());
        assert_eq!(bundle["PreToolUse"][0]["matcher"], "*");
    }

    /// **Every verb this family's adapter declares is registered, in the shape
    /// its own page documents for that verb.** The bundle is derived from the
    /// adapter rather than from a list kept here, so a verb Rift learns to
    /// answer is a verb the bundle registers — and the two invocation
    /// boundaries are the reason that matters now: the hook before an
    /// invocation is where this family is handed what it is missing, and the
    /// hook after one is acknowledged and never injected into.
    ///
    /// The shape is per-verb and its page says so plainly: the tool verbs take
    /// a matcher over a nested handler list, and the rest take the handlers
    /// directly, because for those the matcher is ignored. Registering a flat
    /// verb under a matcher would be a fragment its client cannot read.
    #[test]
    fn every_verb_the_antigravity_adapter_declares_is_registered_in_its_own_shape() {
        let emitted = hook_fragment(ClientFamily::Antigravity, Path::new("/opt/rift/bin/rift"));
        let bundle = emitted.as_object().expect("the bundle Rift reserves");
        let declared = standard_events(ClientFamily::Antigravity);
        assert!(
            declared.contains(&"PreInvocation") && declared.contains(&"PostInvocation"),
            "the boundary verbs are the adapter's to declare: {declared:?}"
        );
        for verb in &declared {
            let group = bundle
                .get(*verb)
                .and_then(|group| group.as_array())
                .unwrap_or_else(|| panic!("{verb} is declared but not registered"));
            assert_eq!(group.len(), 1, "{verb} registers one handler group");
            let entry = &group[0];
            if is_tool_event(verb) {
                assert_eq!(entry["matcher"], "*", "{verb}");
                assert_eq!(entry["hooks"][0]["type"], "command", "{verb}");
                assert!(
                    entry["hooks"][0]["command"]
                        .as_str()
                        .is_some_and(|command| command.ends_with(&format!("antigravity {verb}"))),
                    "{verb} runs Rift's own hook for that verb"
                );
            } else {
                assert!(
                    entry.get("matcher").is_none(),
                    "{verb} ignores the matcher, so the fragment carries none"
                );
                assert_eq!(entry["type"], "command", "{verb}");
                assert!(
                    entry["command"]
                        .as_str()
                        .is_some_and(|command| command.ends_with(&format!("antigravity {verb}"))),
                    "{verb} runs Rift's own hook for that verb"
                );
            }
        }
        // Nothing but the declared verbs and the bundle's own flag.
        let mut keys: Vec<&str> = bundle.keys().map(String::as_str).collect();
        keys.retain(|key| *key != "enabled");
        keys.sort_unstable();
        let mut expected = declared.clone();
        expected.sort_unstable();
        assert_eq!(keys, expected, "the bundle registers what the adapter says");
        assert_eq!(bundle["enabled"], serde_json::Value::Bool(true));
    }

    /// **The limit written into a hook file is the limit the hook fits its text
    /// to** — one number, read here from the emission that lands on disk and
    /// compared against the table the hook reads.
    ///
    /// Written on the two boundary handlers that carry context and on no other:
    /// the key means nothing on a tool handler, and a number written where it
    /// means nothing is a number a later reader has to disprove.
    #[test]
    fn the_context_limit_written_into_a_hook_file_is_the_one_the_hook_assumes() {
        let hooks = hook_fragment(ClientFamily::Codex, Path::new("/opt/rift/bin/rift"));
        let expected = u64::from(rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS);
        for verb in &standard_events(ClientFamily::Codex) {
            let handler = &hooks[*verb][0]["hooks"][0];
            let written = handler["additionalContextLimit"].as_u64();
            assert_eq!(
                written,
                rift_agent::respond::declared_context_limit(
                    &rift_core::AgentId::new(ClientFamily::Codex.token()),
                    verb
                )
                .map(u64::from),
                "{verb}: the file and the table must name the same bound"
            );
            if matches!(*verb, "UserPromptSubmit" | "SessionStart") {
                assert_eq!(written, Some(expected), "{verb} carries the bound");
            } else {
                assert!(written.is_none(), "{verb} has no context channel to bound");
            }
        }
        // No other family declares one, so no other family's emission carries
        // the key — an absence worth asserting, because writing it everywhere
        // would also pass the equality above.
        for other in [ClientFamily::ClaudeCode, ClientFamily::Antigravity] {
            let emitted = hook_fragment(other, Path::new("/opt/rift/bin/rift")).to_string();
            assert!(
                !emitted.contains("additionalContextLimit"),
                "{}: {emitted}",
                other.token()
            );
        }
    }

    #[test]
    fn stale_codex_and_external_readiness_are_actionable() {
        assert_eq!(parse_codex_minor("codex-cli 0.25.0"), Some(25));
        let stale = readiness_action(
            ClientFamily::Codex,
            DetectionState::Unsupported,
            WiringState::Configured,
            ConnectionState::NeverSeen,
            Some("codex-cli 0.25.0"),
        )
        .unwrap();
        assert!(stale.contains("upgrade Codex"));

        let blocked = readiness_action(
            ClientFamily::Gemini,
            DetectionState::Supported,
            WiringState::Configured,
            ConnectionState::NeverSeen,
            Some("0.35.3"),
        )
        .unwrap();
        assert!(blocked.contains("authenticate Gemini CLI"));
    }
}
