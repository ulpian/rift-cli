//! What a terminal result showed the agent, recorded as lines of the files it
//! carried: the `matched` rows of the window journal.
//!
//! Separate because it is the one journal step that reads a call's *output*.
//! Every other step records what a call asked for; this one finds the
//! delivered lines in the files the command line named, at the bytes on disk
//! when the result arrived, and books each stretch to the one member of the
//! line that owns it. Nothing is decided here: the rows are coverage only
//! while their lever is at `enforce`, and below it they size the pool.
//!
//! The work is a hook's: the files named (hashed exactly as the read gate
//! hashes them), the family's line-hash cache or the bytes in hand, and one
//! append per booking. No store, no git object database, no tokenizer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rift_agent::window_journal::output_match::{
    chain_members, file_line_hashes, match_delivery, printed_paths, Booking, ChainMember, Matching,
    Owners,
};
use rift_agent::window_journal::{matched_lines, Mode, DEFAULT_MAX_SHELL_DELIVERED_BYTES};

use super::delivery::{PERSISTED_OUTPUT_MARKER, PERSISTED_OUTPUT_PATH};
use super::file::{gate_rel_path, GateFile};
use super::held_output::{held_output_record, HeldCall};
use super::linehash_cache::load_linehash_cache;
use super::{gate_mode, gate_turn, payload_cwd};

/// The most files one delivery is matched against: the files its members
/// name, then the files its own printed paths name under a named directory.
const MAX_CANDIDATE_FILES: usize = 16;

/// The most words of a command line looked at as names.
const MAX_NAME_WORDS: usize = 256;

/// The most distinct printed prefixes resolved against the filesystem.
const MAX_PRINTED_PREFIXES: usize = 64;

/// What the members of one command line name.
#[derive(Default)]
struct Names {
    /// For each member, the files it names and the directories it names, as
    /// this checkout spells them (a directory with a trailing `/`, the root
    /// as the empty string).
    members: Vec<(Vec<String>, Vec<String>)>,
    /// For each member, whether it changes directory.
    cd: Vec<bool>,
    /// The directories the members ran in, absolute, for reading a printed
    /// relative path.
    dirs: Vec<PathBuf>,
}

/// The files a delivery is matched against.
struct Candidates {
    files: Vec<GateFile>,
    hashes: Vec<Vec<u64>>,
    printed: HashMap<String, usize>,
}

/// Record what a Claude Code `Bash` result showed this window, under the
/// resolved lever `recognise_by_output`, and book what substituting its held
/// lines would save under `substitute_held_output`. Says whether a row was
/// written.
#[cfg(test)]
pub(super) fn output_match_record(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> bool {
    output_match_step(registry, raw, git_dir, work_tree, effective).0
}

/// `output_match_record`, also handing back the replaced result object when
/// the held-output lever enforces and at least one stretch was stood in for.
pub(super) fn output_match_step(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> (bool, Option<serde_json::Value>) {
    if effective.recognise_by_output == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return (false, None);
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return (false, None);
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let Some((command, output)) = bash_delivery(raw, facts.tool_name.as_deref()) else {
        return (false, None);
    };
    let (Ok(root), Some(cwd)) = (work_tree.canonicalize(), payload_cwd(&facts)) else {
        return (false, None);
    };
    let Some(members) = chain_members(&command) else {
        return (false, None);
    };
    let names = chain_names(&root, &cwd, &members);
    let hashing = Hashing {
        root: &root,
        git_dir,
        max_bytes: effective.max_inline_hash_bytes,
    };
    let candidates = candidate_files(&hashing, &names, &output);
    let bookings = match_candidates(&names, &candidates, &output);
    let journal = crate::window_journal::WindowJournal::open(
        work_tree,
        &raw.session,
        facts.child_session.as_deref(),
    );
    // Loaded before this result's own rows land, though they could not prove
    // anything anyway: the held proof never counts a matched row.
    let held = HeldCall {
        git_dir,
        work_tree,
        raw,
        window: &crate::window_journal::key(&raw.session, facts.child_session.as_deref()),
        mode: gate_mode(effective.substitute_held_output),
    };
    let replaced = if held.mode == Mode::Off {
        None
    } else {
        held_output_record(&held, &journal.load().journal, &candidates.files, &bookings)
    };
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    let mut written = false;
    for booking in &bookings {
        let Some(file) = candidates.files.get(booking.file) else {
            continue;
        };
        let member = u32::try_from(booking.member).unwrap_or(u32::MAX);
        for line in matched_lines(
            &file.rel_path,
            file.hash,
            &booking.runs,
            file.total_lines,
            member,
            turn,
            at_ms,
        ) {
            written |= journal.append(&line).is_ok();
        }
    }
    (written, replaced)
}

/// The command line and the text a `Bash` result handed the model, or
/// `None` for any other call, a result the client kept in a file, or one
/// past the size the client hands over inline.
fn bash_delivery(
    raw: &rift_shadow::hooks::RawHook,
    tool_name: Option<&str>,
) -> Option<(String, String)> {
    if raw.agent != rift_core::AgentId::CLAUDE_CODE
        || raw.verb != "PostToolUse"
        || tool_name != Some("Bash")
    {
        return None;
    }
    let command = raw.payload.get("tool_input")?.get("command")?.as_str()?;
    let response = raw.payload.get("tool_response")?;
    let persisted = response
        .get(PERSISTED_OUTPUT_PATH)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| !path.is_empty());
    let field = |name: &str| response.get(name).and_then(serde_json::Value::as_str);
    let stdout = response
        .as_str()
        .or_else(|| field("stdout"))
        .unwrap_or_default();
    if persisted || stdout.starts_with(PERSISTED_OUTPUT_MARKER) {
        return None;
    }
    let mut output = stdout.to_owned();
    if let Some(stderr) = field("stderr").filter(|text| !text.is_empty()) {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    if output.chars().count() >= DEFAULT_MAX_SHELL_DELIVERED_BYTES {
        return None;
    }
    Some((command.to_owned(), output))
}

/// What each member names, read against the directory it ran in.
///
/// A word names a file or a directory only when it resolves to one of this
/// checkout; an option, a pattern or a command word that happens to name
/// nothing costs nothing. A member whose directory an earlier `cd` left
/// unknown names nothing at all.
fn chain_names(root: &Path, cwd: &Path, members: &[ChainMember]) -> Names {
    let mut names = Names::default();
    let mut budget = MAX_NAME_WORDS;
    for member in members {
        let dir = member
            .dir
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), |d| cwd.join(d));
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        if !member.cd && !member.dir_unknown {
            for word in member.words.iter().skip(1).filter(|w| !w.starts_with('-')) {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                name_of(root, &dir, word, &mut files, &mut dirs);
            }
            if !names.dirs.contains(&dir) {
                names.dirs.push(dir);
            }
        }
        names.members.push((files, dirs));
        names.cd.push(member.cd);
    }
    names
}

/// File or directory: what one word names, if anything.
fn name_of(root: &Path, dir: &Path, word: &str, files: &mut Vec<String>, dirs: &mut Vec<String>) {
    let Some(rel) = gate_rel_path(root, word, Some(dir)) else {
        return;
    };
    let Ok(meta) = std::fs::metadata(root.join(&rel)) else {
        return;
    };
    if meta.is_file() {
        files.push(rel);
    } else if meta.is_dir() {
        let prefix = if rel.is_empty() || rel == "." {
            String::new()
        } else {
            format!("{}/", rel.trim_end_matches('/'))
        };
        dirs.push(prefix);
    }
}

/// Whether a member names `rel`, as a file or under a directory it names.
fn names_file(member: &(Vec<String>, Vec<String>), rel: &str) -> bool {
    member.0.iter().any(|file| file == rel)
        || member.1.iter().any(|dir| rel.starts_with(dir.as_str()))
}

/// Where candidate files are read from, and how large one may be.
struct Hashing<'a> {
    root: &'a Path,
    git_dir: &'a Path,
    max_bytes: u64,
}

/// The index of `rel` among the candidates, hashing it in as the read gate
/// hashes a file when it is not one yet; `None` past the cap or for bytes
/// the gate would not hash.
///
/// A free function rather than a method, so the decision-path walk follows
/// the call into the cache read.
fn add_candidate(hashing: &Hashing<'_>, candidates: &mut Candidates, rel: &str) -> Option<usize> {
    if let Some(at) = candidates.files.iter().position(|f| f.rel_path == rel) {
        return Some(at);
    }
    if candidates.files.len() >= MAX_CANDIDATE_FILES {
        return None;
    }
    let file = candidate_file(hashing.root, rel, hashing.max_bytes)?;
    let byte_count = u64::try_from(file.bytes.len()).unwrap_or(u64::MAX);
    let hashes = load_linehash_cache(hashing.git_dir, file.hash, file.total_lines, byte_count)
        .unwrap_or_else(|| file_line_hashes(&file.bytes));
    candidates.files.push(file);
    candidates.hashes.push(hashes);
    Some(candidates.files.len() - 1)
}

/// The bytes of `rel`, a name this checkout already spells, as the read gate
/// would key them: the same refusals (not a file, past the inline-hash cap,
/// holding a zero) and the same SHA-256 of the bytes.
///
/// Not [`gate_file`](super::file::gate_file): that one resolves a payload's spelling and computes the
/// git object id the worker falls back to, and neither is needed for a match
/// against bytes in hand — a hook that hashed sixteen files twice would spend
/// its budget on a fact nothing reads.
fn candidate_file(root: &Path, rel: &str, max_bytes: u64) -> Option<GateFile> {
    let path = root.join(rel);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let total_lines = bytes.split_inclusive(|byte| *byte == b'\n').count();
    Some(GateFile {
        rel_path: rel.to_owned(),
        hash: rift_agent::content_sha256(&bytes),
        blob_oid: String::new(),
        total_lines: u32::try_from(total_lines).ok()?,
        bytes,
    })
}

/// The named files, then the files the output's printed paths name under a
/// named directory, each hashed as the read gate hashes it.
fn candidate_files(hashing: &Hashing<'_>, names: &Names, output: &str) -> Candidates {
    let mut candidates = Candidates {
        files: Vec::new(),
        hashes: Vec::new(),
        printed: HashMap::new(),
    };
    for (files, _) in &names.members {
        for rel in files {
            add_candidate(hashing, &mut candidates, rel);
        }
    }
    let mut tried: HashSet<&str> = HashSet::new();
    for prefix in printed_prefixes(output) {
        // Past the cap a new file could not be matched against anyway, and
        // each resolution is a walk of the filesystem.
        if candidates.files.len() >= MAX_CANDIDATE_FILES || tried.len() >= MAX_PRINTED_PREFIXES {
            break;
        }
        if !tried.insert(prefix) {
            continue;
        }
        let Some(rel) = printed_file(hashing.root, names, prefix) else {
            continue;
        };
        if let Some(at) = add_candidate(hashing, &mut candidates, &rel) {
            candidates.printed.insert(prefix.to_owned(), at);
        }
    }
    candidates
}

/// Every prefix of a delivered line that could be a printed path, in order:
/// the pure normaliser's own reading of where a path ends.
fn printed_prefixes(output: &str) -> impl Iterator<Item = &str> {
    output.lines().flat_map(printed_paths)
}

/// The checkout file a printed prefix names from one of the members'
/// directories, when some member names it — a printed path no member named
/// is output nobody can own, and is not read.
fn printed_file(root: &Path, names: &Names, prefix: &str) -> Option<String> {
    names.dirs.iter().find_map(|dir| {
        let rel = gate_rel_path(root, prefix, Some(dir))?;
        let owned = names.members.iter().any(|member| names_file(member, &rel));
        (owned && root.join(&rel).is_file()).then_some(rel)
    })
}

/// Match the delivery against the candidates and book it to its members.
fn match_candidates(names: &Names, candidates: &Candidates, output: &str) -> Vec<Booking> {
    let named_by: Vec<Vec<usize>> = candidates
        .files
        .iter()
        .map(|file| {
            names
                .members
                .iter()
                .enumerate()
                .filter(|(_, member)| names_file(member, &file.rel_path))
                .map(|(at, _)| at)
                .collect()
        })
        .collect();
    let named_files: Vec<&String> = names.members.iter().flat_map(|m| &m.0).collect();
    let sole_file = match named_files.as_slice() {
        [only] => candidates.files.iter().position(|f| &f.rel_path == *only),
        _ => None,
    };
    let slices: Vec<&[u64]> = candidates.hashes.iter().map(Vec::as_slice).collect();
    let matching = Matching {
        files: &slices,
        owners: Owners {
            named_by: &named_by,
            cd: &names.cd,
        },
        sole_file,
    };
    match_delivery(&matching, output, &|printed| {
        candidates.printed.get(printed).copied()
    })
}
