//! Byte goldens for the standing text an agent reads, and the checks that
//! every copy of it equals its source.
//!
//! A separate file because the goldens are data: beside the agent-help map's
//! golden they would put `agent_help.rs` over the file ceiling. They are
//! byte goldens rather than shape checks because a changed byte in the
//! pointer or the skill body is a different prompt prefix for every session
//! that holds it, and a prefix change is paid for in cache writes by every
//! one of those sessions.

use super::{session_pointer, skill_body};

/// The once-per-session pointer, byte for byte.
const POINTER_GOLDEN: &str = "\
Rift knows what changed in this repository since you last read it. Before
re-reading a file, ask: rift recall known-state --task <goal> --file <path> --json
Full map: rift agent-help";

/// The managed skill body, byte for byte.
const SKILL_BODY_GOLDEN: &str = "\
Rift automatically captures this repository and delivers bounded recall context through native hooks.
- `rift agent-help` — inspect the live recall and recovery surface.
- `rift recall status --json` — verify hook, runtime, context, and degradation status.
Documents (.docx, .pdf, .xlsx and the like) are converted once for this repository and kept as Markdown under `.rift/state/doc/`; a hook points a document read there, or names the file to read in its place. Read that Markdown rather than the binary, or ask `rift recall doc-read <path>` for it; where there is none, read the document as usual.
If Rift reports degradation, let the raw agent call run and use the diagnostic above; correctness never depends on this file.";

/// The serving chapter, whose section on the pointer quotes it.
const SERVING_CHAPTER: &str = include_str!("../../../../docs/guide/07-serving-and-injection.md");

/// The heading of the section whose first fenced block is the pointer.
const POINTER_SECTION: &str = "## 7.2 Tier one: the session pointer\n";

/// The pointer's bytes are pinned: a reworded pointer is a new prefix for
/// every session, so it has to arrive as a reviewed change to this golden.
#[test]
fn the_session_pointer_is_byte_identical_to_its_golden() {
    assert_eq!(session_pointer(), POINTER_GOLDEN);
}

/// The skill body's bytes are pinned for the same reason as the pointer's:
/// every managed region an agent reads is rendered from it.
#[test]
fn the_skill_body_is_byte_identical_to_its_golden() {
    assert_eq!(skill_body(), SKILL_BODY_GOLDEN);
}

/// The region the installer writes holds exactly the skill body, for every
/// adapter it writes one for. The adapters are read from the installer's own
/// answer (a region-bearing adapter is one it can report a region for), so a
/// channel added there is covered here without a second list to forget.
#[test]
fn every_written_region_holds_exactly_the_skill_body() {
    let body = skill_body();
    let mut covered = Vec::new();
    for token in rift_agent_install::native::ALPHA_AGENTS {
        let dir = tempfile::tempdir().unwrap();
        if rift_agent_install::native::recall_skill_present(dir.path(), token).is_none() {
            continue;
        }
        let rendered = rift_agent_install::native::skill_region(token, &body);
        assert_eq!(
            fenced_body(&rendered, token),
            Some(body.trim_end()),
            "{token}"
        );
        rift_agent_install::native::install_recall_skill(dir.path(), token, &body).unwrap();
        assert!(
            written_files(dir.path())
                .iter()
                .any(|text| text.contains(&rendered)),
            "{token}: the file on disk does not carry the rendered region"
        );
        covered.push(*token);
    }
    assert_eq!(covered, ["claude-code", "codex", "antigravity"]);
}

/// The guide's quotation of the pointer is checked, not trusted: a copy that
/// drifts teaches a reader a pointer no agent is ever shown.
#[test]
fn the_guide_quotes_the_session_pointer_exactly() {
    let lf = SERVING_CHAPTER.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    for chapter in [&lf, &crlf] {
        let excerpt =
            guide_pointer_excerpt(chapter).expect("section 7.2 opens with a fenced block");
        assert_eq!(excerpt, session_pointer());
    }
}

/// The text between a region's two fences, without the newlines that
/// separate it from them.
fn fenced_body<'a>(region: &'a str, token: &str) -> Option<&'a str> {
    let begin = format!("<!-- RIFT-MANAGED:{token}:begin -->\n");
    let end = format!("\n<!-- RIFT-MANAGED:{token}:end -->\n");
    region.strip_prefix(&begin)?.strip_suffix(&end)
}

/// Every file under `root`, read as text, at any depth.
fn written_files(root: &std::path::Path) -> Vec<String> {
    let mut texts = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                texts.push(std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    texts
}

/// The first fenced block after the pointer section's heading.
fn guide_pointer_excerpt(chapter: &str) -> Option<String> {
    let normalized = chapter.replace("\r\n", "\n");
    let section = normalized.split_once(POINTER_SECTION)?.1;
    let block = section.split_once("```\n")?.1;
    Some(block.split_once("\n```")?.0.to_owned())
}
