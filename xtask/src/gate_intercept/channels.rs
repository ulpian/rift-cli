//! The `pinned-channels` leg: the channels Rift answers on are reviewable as a
//! list.

use std::collections::BTreeSet;
use std::path::Path;

use crate::ci_meta::tracked_paths;

use super::Leg;

/// Where the reviewable channel list lives.
const CHANNELS_FILE: &str = "ci/intercept-channels.txt";

/// The fence a pinned output-surface artifact keeps its capability rows in.
const CAPABILITY_FENCE: &str = "```rift-capability-rows";

/// **The channels Rift answers on are a reviewable list.**
///
/// `ci/intercept-channels.txt` must equal, as a set, the capability rows fenced
/// in every tracked `*-hook-output-surface.md`.
///
/// The chain this closes, and where each link is proven: the code table
/// (`rift_agent::respond::CAPABILITIES`) equals its family's fixture rows —
/// proven by each adapter's own `pinned_output_surface` test, which renders the
/// table and compares both directions — and the fixture rows equal this file,
/// proven here. So a widened channel cannot reach the tree without touching the
/// artifact AND this list, and the list is one place a reviewer can read.
///
/// The limit, stated: this leg does not parse `CAPABILITIES` itself. It reads
/// the artifacts, which are the authority the table is checked against. If the
/// adapter tests were deleted the chain would lose its middle link, which is
/// why those tests are named in the program record rather than left implicit.
///
/// # Errors
/// If the list or an artifact cannot be read, or no artifact is tracked.
pub fn pinned_channels(root: &Path) -> Result<Leg, String> {
    let (pinned, fixtures) = pinned_channel_rows(root)?;
    Ok(screen_channels(
        &stated_channel_rows(root)?,
        &pinned,
        fixtures,
    ))
}

/// The comparison itself, over two row sets — the seam a shape test uses.
fn screen_channels(stated: &BTreeSet<String>, pinned: &BTreeSet<String>, fixtures: usize) -> Leg {
    let mut v = Vec::new();
    for row in stated.difference(pinned) {
        v.push(format!(
            "{CHANNELS_FILE}: `{row}` is stated here but no pinned artifact carries it"
        ));
    }
    for row in pinned.difference(stated) {
        v.push(format!(
            "{CHANNELS_FILE}: a pinned artifact carries `{row}` and this list does not — \
             a widened channel is reviewed here or it is not reviewed"
        ));
    }
    Leg::new(
        "pinned-channels",
        format!(
            "{} row(s) across {fixtures} pinned artifact(s) match {CHANNELS_FILE}",
            pinned.len()
        ),
        v,
    )
}

/// The rows every pinned output-surface artifact carries, each prefixed with
/// the family its adapter crate is named for.
fn pinned_channel_rows(root: &Path) -> Result<(BTreeSet<String>, usize), String> {
    let mut rows = BTreeSet::new();
    let mut fixtures = 0_usize;
    for rel in tracked_paths(root)? {
        let Some(name) = rel.rsplit('/').next() else {
            continue;
        };
        if !name.ends_with("-hook-output-surface.md") {
            continue;
        }
        let family = rel
            .split('/')
            .nth(1)
            .and_then(|c| c.strip_prefix("rift-agent-"))
            .ok_or_else(|| format!("{rel}: not inside an adapter crate"))?
            .to_owned();
        let text = std::fs::read_to_string(root.join(&rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        fixtures += 1;
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim_end();
            if inside {
                if line == "```" {
                    inside = false;
                } else if !line.trim().is_empty() {
                    rows.insert(format!("family={family} {}", line.trim()));
                }
            } else if line == CAPABILITY_FENCE {
                inside = true;
            }
        }
    }
    if fixtures == 0 {
        return Err("gate-intercept: no pinned output-surface artifact is tracked".to_owned());
    }
    Ok((rows, fixtures))
}

/// The rows `ci/intercept-channels.txt` states, comments and blanks dropped.
fn stated_channel_rows(root: &Path) -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(root.join(CHANNELS_FILE))
        .map_err(|e| format!("read {CHANNELS_FILE}: {e}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ A channel widened in a pinned artifact but not in the reviewable list
    /// is a finding, and so is the reverse.
    #[test]
    fn a_channel_the_reviewable_list_does_not_state_is_caught() {
        let stated: BTreeSet<String> = ["family=codex verb=PreCompact channel=Neutral"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut pinned = stated.clone();
        pinned.insert("family=codex verb=PreToolUse channel=DenyWithReason".to_owned());
        let leg = screen_channels(&stated, &pinned, 1);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("and this list does not"));

        let leg = screen_channels(&pinned, &stated, 1);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("no pinned artifact carries it"));
    }

    /// The reviewable list is the union of the artifacts, family by family —
    /// checked against the tree rather than against a second copy of itself.
    #[test]
    fn the_reviewable_list_names_every_family_with_a_pinned_artifact() {
        let root = crate::workspace_root();
        let (pinned, fixtures) = pinned_channel_rows(&root).expect("the artifacts parse");
        assert!(fixtures >= 3, "three alpha families are pinned: {fixtures}");
        for family in ["claude-code", "codex", "antigravity"] {
            assert!(
                pinned
                    .iter()
                    .any(|r| r.starts_with(&format!("family={family} "))),
                "{family} has rows"
            );
        }
        assert_eq!(stated_channel_rows(&root).expect("the list parses"), pinned);
    }
}
