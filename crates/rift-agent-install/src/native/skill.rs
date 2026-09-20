//! The Recall skill region: the instructions file per adapter, the fence Rift
//! owns inside it, and the frontmatter a channel needs ahead of the fence.

use std::path::Path;

use crate::{atomic_write, InstallError};

use super::manifest::{fingerprint, manifest_path, read_manifest, write_manifest, OwnedSkill};
use super::{is_alpha_supported, remove_file, NativeIntegrationReport, ALPHA_AGENTS};

/// Install the Recall pointer for one enabled native adapter. Shared agent
/// instruction files keep user content around a token-specific managed fence;
/// agent-specific files are never adopted when an unmarked file already exists.
///
/// # Errors
/// Refuses an unowned file at a Rift-specific path and reports I/O failures.
pub fn install_recall_skill(
    repo: &Path,
    agent_token: &str,
    body: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    if !is_alpha_supported(agent_token) {
        return Err(InstallError::Config {
            path: repo.join(".rift/agents"),
            reason: format!(
                "recall rules for {agent_token} are coming soon; this alpha supports {}",
                ALPHA_AGENTS.join(", ")
            ),
        });
    }
    let Some(rel) = skill_path(agent_token) else {
        return Err(InstallError::Config {
            path: repo.join(".rift/agents"),
            reason: "no managed skill channel exists for this adapter".to_owned(),
        });
    };
    let path = repo.join(rel);
    let region = skill_region(agent_token, body);
    let existing = read_optional_text(&path)?;
    let next = next_skill_text(&path, existing.as_deref(), agent_token, &region)?;
    let mut changed = Vec::new();
    if existing.as_deref() != Some(next.as_str()) {
        atomic_write(&path, next.as_bytes())?;
        changed.push(path);
    }
    let mut manifest = read_manifest(repo);
    manifest.skills.insert(
        agent_token.to_owned(),
        OwnedSkill {
            path: rel.to_owned(),
            region_hash: fingerprint(region.as_bytes()),
        },
    );
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

/// Remove only the fingerprint-matching Recall block for one adapter.
///
/// # Errors
/// Refuses a modified managed block and leaves it untouched.
pub fn remove_recall_skill(
    repo: &Path,
    agent_token: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    let mut manifest = read_manifest(repo);
    let Some(owned) = manifest.skills.get(agent_token).cloned() else {
        return Ok(NativeIntegrationReport::default());
    };
    let path = repo.join(&owned.path);
    let text = read_optional_text(&path)?.unwrap_or_default();
    let mut changed = Vec::new();
    if !text.is_empty() {
        remove_skill_region(&path, &text, agent_token, &owned.region_hash)?;
        changed.push(path);
    }
    manifest.skills.remove(agent_token);
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

/// Whether this checkout carries the Recall skill region for `agent_token`.
/// `None` when the adapter has no skill file at all.
///
/// Read-only, and that is the whole point: the skill files are `AGENTS.md`,
/// `.agents/rules/rift-recall.md` and their siblings — files the repository
/// TRACKS. Adoption reports a missing region and leaves the explicit
/// `rift enable <agent>` path to write it, because a git hook that dirties the
/// tree the user just created is not a repair.
#[must_use]
pub fn recall_skill_present(repo: &Path, agent_token: &str) -> Option<bool> {
    let relative = skill_path(agent_token)?;
    Some(
        std::fs::read_to_string(repo.join(relative))
            .is_ok_and(|text| skill_fence(&text, agent_token).is_some()),
    )
}

/// The file after the install: a fresh file leads with the channel's
/// frontmatter; a fenced file has its region replaced; a shared instructions
/// file without a fence gains one at its end; an unmarked file at an
/// agent-specific path is somebody else's and is refused.
fn next_skill_text(
    path: &Path,
    existing: Option<&str>,
    token: &str,
    region: &str,
) -> Result<String, InstallError> {
    let Some(text) = existing else {
        return Ok(format!(
            "{}{region}",
            skill_frontmatter(token).unwrap_or_default()
        ));
    };
    match skill_fence(text, token) {
        Some((start, end)) => Ok(format!("{}{}{}", &text[..start], region, &text[end..])),
        None if skill_is_shared(token) => {
            let separator = if text.ends_with('\n') { "\n" } else { "\n\n" };
            Ok(format!("{text}{separator}{region}"))
        }
        None => Err(InstallError::Config {
            path: path.to_path_buf(),
            reason: "an unowned file exists at Rift's managed skill path".to_owned(),
        }),
    }
}

/// Cut Rift's fenced region out of `text`, refusing a fence that is missing,
/// malformed, or no longer hashes to the recorded fingerprint. A shared file
/// keeps what remains; a file Rift created goes whole.
fn remove_skill_region(
    path: &Path,
    text: &str,
    token: &str,
    region_hash: &str,
) -> Result<(), InstallError> {
    let refuse = |reason: &str| InstallError::Config {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    };
    let Some((start, end)) = skill_fence(text, token) else {
        return Err(refuse("managed skill fence is missing"));
    };
    let Some(region) = text.get(start..end) else {
        return Err(refuse("managed skill fence is invalid"));
    };
    if fingerprint(region.as_bytes()) != region_hash {
        return Err(refuse(
            "managed skill block differs from Rift's ownership fingerprint",
        ));
    }
    let rest = format!("{}{}", &text[..start], &text[end..]);
    if skill_is_shared(token) && !rest.trim().is_empty() {
        atomic_write(path, format!("{}\n", rest.trim_end()).as_bytes())
    } else {
        remove_file(path)
    }
}

fn read_optional_text(path: &Path) -> Result<Option<String>, InstallError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The instructions file Rift writes its Recall region into, per adapter.
///
/// The table names exactly the tokens [`install_recall_skill`] will write for —
/// the agents this alpha captures. A path defined for a token the writer
/// refuses is a channel that cannot be reached: it reads as support and is
/// not.
fn skill_path(token: &str) -> Option<&'static str> {
    match token {
        "claude-code" => Some(".claude/skills/rift-recall/SKILL.md"),
        "codex" => Some("AGENTS.md"),
        "antigravity" => Some(".agents/rules/rift-recall.md"),
        _ => None,
    }
}

/// Whether the file at [`skill_path`] is one the repository already uses for
/// its own instructions, so Rift writes inside a fence and never owns the
/// whole document.
fn skill_is_shared(token: &str) -> bool {
    token == "codex"
}

/// The YAML frontmatter a family's channel requires ahead of Rift's fence.
///
/// Claude Code discovers a skill by this block, so it has to lead the file —
/// which is why the fence wraps the body and not the whole document. Written
/// once, when Rift creates the file: a re-install rewrites only what is
/// between the markers, so a user who edits the description keeps it, and
/// `remove_recall_skill` deletes the whole file Rift created, frontmatter
/// included.
///
/// This is the one live fact the retired `skills.rs` held and this path never
/// did; it is ported here rather than deleted with it.
fn skill_frontmatter(token: &str) -> Option<&'static str> {
    match token {
        "claude-code" => Some(
            "---\n\
             name: rift-recall\n\
             description: Ask Rift what changed since the state you already hold, \
             instead of re-reading files. Use at the start of a turn, before reading \
             source you may already have.\n\
             ---\n\n",
        ),
        _ => None,
    }
}

fn skill_region(token: &str, body: &str) -> String {
    format!(
        "<!-- RIFT-MANAGED:{token}:begin -->\n{}\n<!-- RIFT-MANAGED:{token}:end -->\n",
        body.trim_end()
    )
}

fn skill_fence(text: &str, token: &str) -> Option<(usize, usize)> {
    let begin = format!("<!-- RIFT-MANAGED:{token}:begin -->");
    let close = format!("<!-- RIFT-MANAGED:{token}:end -->");
    let start = text.find(&begin)?;
    let close_start = start + text.get(start..)?.find(&close)?;
    let mut end = close_start + close.len();
    if text.get(end..).is_some_and(|rest| rest.starts_with('\n')) {
        end += 1;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Recall skill regions live in TRACKED files (`AGENTS.md`,
    /// `.agents/rules/rift-recall.md`), so adoption reports them and the
    /// explicit `rift enable` path writes them. This is the reporting half.
    #[test]
    fn the_recall_skill_region_is_reported_not_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(false));
        assert_eq!(
            recall_skill_present(dir.path(), "not-a-family"),
            None,
            "an adapter with no skill file has no answer to give"
        );

        let tracked = dir.path().join("AGENTS.md");
        std::fs::write(&tracked, "# house rules\n").unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(false));
        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "# house rules\n",
            "asking is not writing"
        );

        install_recall_skill(dir.path(), "codex", "ask rift first").unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(true));
    }

    #[test]
    fn recall_skills_are_per_adapter_and_fingerprint_safe() {
        let dir = tempfile::tempdir().unwrap();
        let user = "# User rules\n";
        std::fs::write(dir.path().join("AGENTS.md"), user).unwrap();
        install_recall_skill(dir.path(), "codex", "Ask `rift agent-help`.").unwrap();
        let path = dir.path().join("AGENTS.md");
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(installed.starts_with(user));
        assert!(installed.contains("RIFT-MANAGED:codex:begin"));
        remove_recall_skill(dir.path(), "codex").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), user);

        install_recall_skill(dir.path(), "antigravity", "Ask Rift.").unwrap();
        let rules = dir.path().join(skill_path("antigravity").unwrap());
        std::fs::write(
            &rules,
            std::fs::read_to_string(&rules)
                .unwrap()
                .replace("Ask Rift.", "user edit"),
        )
        .unwrap();
        assert!(remove_recall_skill(dir.path(), "antigravity").is_err());
        assert!(rules.exists());
    }

    /// Claude Code discovers a skill by its YAML frontmatter, so a `SKILL.md`
    /// without one is a file the agent never reads. The frontmatter leads the
    /// file, the fence follows it, a re-install rewrites only the fence, and
    /// removal takes the whole file Rift created.
    #[test]
    fn the_claude_code_skill_leads_with_the_frontmatter_that_makes_it_discoverable() {
        let dir = tempfile::tempdir().unwrap();
        install_recall_skill(dir.path(), "claude-code", "Ask `rift agent-help`.").unwrap();
        let path = dir.path().join(skill_path("claude-code").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\nname: rift-recall\n"), "{text}");
        assert!(
            text.contains("\n---\n\n<!-- RIFT-MANAGED:claude-code:begin -->"),
            "the fence must follow the frontmatter: {text}"
        );

        install_recall_skill(dir.path(), "claude-code", "A different body.").unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            again.matches("name: rift-recall").count(),
            1,
            "a re-install duplicated the frontmatter: {again}"
        );
        assert!(again.starts_with("---\n"), "{again}");
        assert!(again.contains("A different body."), "{again}");
        assert!(!again.contains("Ask `rift agent-help`."), "{again}");

        remove_recall_skill(dir.path(), "claude-code").unwrap();
        assert!(
            !path.exists(),
            "Rift created the file and must remove it whole"
        );
    }

    /// The never-clobber rule, on a path Rift owns the NAME of but not the
    /// file at it. Ported from the retired `skills.rs`, which held the ONLY
    /// assertion of this behaviour anywhere in the workspace.
    ///
    /// Measured before porting, not assumed: the refusal's reason string
    /// occurs exactly twice in the tree — once in `install_recall_skill`'s
    /// docstring and once in the arm itself — and no test named it. Replacing
    /// the `None =>` arm with a silent write left the whole workspace green
    /// while the function overwrote a file it does not own, which is the
    /// behaviour both its own docstring and this module's header forbid.
    ///
    /// The assertions are on the TREE rather than on the `Err`, for the same
    /// reason the alpha-refusal test is: the failure worth guarding is not a
    /// missing refusal but one arriving after a write.
    #[test]
    fn an_unowned_file_at_a_rift_owned_skill_path_is_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(skill_path("claude-code").unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mine = "---\nname: mine\n---\n\nMy own skill, thanks.\n";
        std::fs::write(&path, mine).unwrap();

        let error = install_recall_skill(dir.path(), "claude-code", "Ask `rift agent-help`.")
            .expect_err("an unowned file must be refused, never adopted");
        assert!(error.to_string().contains("unowned file"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            mine,
            "a file Rift does not own was rewritten"
        );

        // The refusal has to land before ownership is recorded: a manifest
        // entry for a file Rift never wrote would let `remove_recall_skill`
        // delete somebody else's file later.
        assert!(
            !read_manifest(dir.path()).skills.contains_key("claude-code"),
            "a refused install claimed ownership"
        );

        // And removal leaves it exactly as it is.
        remove_recall_skill(dir.path(), "claude-code").unwrap();
        assert!(path.exists(), "a user's file was deleted");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    }

    /// The channels Rift does not own a discovery contract for get no
    /// frontmatter at all: an unowned prefix on a shared instructions file
    /// would be Rift writing outside its fence.
    #[test]
    fn only_the_claude_code_channel_gets_a_frontmatter_prefix() {
        for token in ["codex", "antigravity"] {
            let dir = tempfile::tempdir().unwrap();
            install_recall_skill(dir.path(), token, "Ask Rift.").unwrap();
            let text =
                std::fs::read_to_string(dir.path().join(skill_path(token).unwrap())).unwrap();
            assert!(
                text.starts_with("<!-- RIFT-MANAGED:"),
                "{token} gained a prefix Rift does not own: {text}"
            );
        }
    }

    #[test]
    fn coming_soon_agents_receive_no_recall_file_or_manifest_claim() {
        for token in [
            "gemini",
            "cursor",
            "copilot-cli",
            "opencode",
            "factory-droid",
            "ollama",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let error = install_recall_skill(dir.path(), token, "Ask Rift.")
                .expect_err("a coming-soon adapter must be refused before writing");
            assert!(
                error.to_string().contains("coming soon"),
                "{token}: {error}"
            );
            assert_eq!(
                skill_path(token),
                None,
                "{token} keeps an instructions path the writer refuses to use"
            );
            assert!(
                !manifest_path(dir.path()).exists(),
                "{token} received an ownership manifest"
            );
        }
    }
}
