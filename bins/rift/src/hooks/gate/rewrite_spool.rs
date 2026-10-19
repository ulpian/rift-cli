//! Keeping the original of a terminal result before a rewrite of it goes out.
//!
//! Q72 lets a hook deliver a Bash result other than the one the tool wrote
//! only when the original is spooled first, under the form the rewrite takes.
//! Both rewrites of a result (held lines, folds) come through here, so the
//! rule has one place to hold: the rewrite is checked against its original
//! with the same check the archive's verify runs, then the original is
//! spooled, and only when both pass may the caller deliver it. Any failure
//! means the result goes out unchanged.
//!
//! The family is read, never minted: a hook that finds no family identity
//! has nowhere its archive would look, so it rewrites nothing.

use std::path::Path;

use rift_core::{FamilyIds as _, SecretBytes};
use rift_shadow::backend::rewrite::{
    check_rendering, LineRange, RenderCheck, ResultText, RewriteForm,
};
use serde_json::Value;

#[cfg(not(test))]
use crate::hooks::originals::spool_original;
use crate::hooks::originals::{RewriteSite, ToolOriginal};

/// The call whose result is about to be rewritten.
pub(super) struct ResultCall<'a> {
    pub(super) raw: &'a rift_shadow::hooks::RawHook,
    pub(super) work_tree: &'a Path,
    /// The window journal's key of the window that ran the command.
    pub(super) window: &'a str,
    /// The subagent whose window ran it, `None` for the session's own.
    pub(super) agent: Option<&'a str>,
}

/// Check the delivered `stdout` against the result's original under `form`
/// and spool the original; says whether the rewrite may be delivered.
pub(super) fn spool_rewrite(
    call: &ResultCall<'_>,
    form: RewriteForm,
    replaced: &[LineRange],
    delivered_stdout: &str,
) -> bool {
    spool_checked(call, form, replaced, delivered_stdout).is_some()
}

/// [`spool_rewrite`], `None` at the first step that refuses.
fn spool_checked(
    call: &ResultCall<'_>,
    form: RewriteForm,
    replaced: &[LineRange],
    delivered_stdout: &str,
) -> Option<()> {
    let response = call.raw.payload.get("tool_response")?;
    let stdout = response.get("stdout")?.as_str()?;
    let stderr = response
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let original = ResultText { stdout, stderr };
    let delivered = ResultText {
        stdout: delivered_stdout,
        stderr,
    };
    // The rewrite is held to the record before anything is kept: a rewrite
    // verify would call a mismatch is one that never goes out.
    if check_rendering(form, original, delivered, replaced) == RenderCheck::Mismatch {
        return None;
    }
    let tool_use_id = call.raw.payload.get("tool_use_id")?.as_str()?;
    let (repo, rift_git::FamilyState::Minted(family)) =
        rift_git::family_of(call.work_tree, rift_git::MintPolicy::Never).ok()?
    else {
        return None;
    };
    let site = RewriteSite {
        repo: &repo,
        session_id: &call.raw.session,
        agent_id: call.agent,
        window_key: call.window,
        family_key: family.family_id(),
        tool_use_id,
        form,
        replaced,
    };
    let kept = ToolOriginal {
        stdout: SecretBytes::new(stdout.as_bytes().to_vec()),
        stderr: SecretBytes::new(stderr.as_bytes().to_vec()),
        interrupted: response
            .get("interrupted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    // Tests spool against a clock that never advances, so no verdict of
    // theirs turns on how fast the machine redacts.
    #[cfg(not(test))]
    let spooled = spool_original(&site, kept);
    #[cfg(test)]
    let spooled = crate::hooks::originals::spool_untimed(&site, &kept);
    spooled.ok().map(|_| ())
}

/// The result object with its `stdout` replaced by `stdout`.
pub(super) fn with_stdout(raw: &rift_shadow::hooks::RawHook, stdout: String) -> Option<Value> {
    let mut output = raw.payload.get("tool_response")?.clone();
    *output.get_mut("stdout")? = Value::from(stdout);
    Some(output)
}
