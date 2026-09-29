//! Detector (c): the binary each agent's hook file runs.
//!
//! A hook whose binary is gone fails before Rift sees anything, so capture is
//! lost in silence; a hook running a binary other than the family's record
//! still captures, but two builds are then writing one ledger. Paths are
//! compared canonicalised, because the record and the hook file may spell one
//! file through different links.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{Finding, Reason};

/// One finding per agent whose wired binary is missing, or present and not
/// the binary of record. Silent where no binary is recorded: there is nothing
/// to disagree with.
pub(super) fn binary_findings(
    hook_binaries: &BTreeMap<String, PathBuf>,
    binary_of_record: Option<&Path>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (agent, wired) in hook_binaries {
        if !rift_shadow_hooks::is_executable_file(wired) {
            findings.push(Finding::new(
                Reason::HookBinaryMissing,
                agent,
                None,
                wired.display().to_string(),
            ));
            continue;
        }
        let Some(record) = binary_of_record else {
            continue;
        };
        if canonical(wired) != canonical(record) {
            findings.push(Finding::new(
                Reason::HookBinaryDisagrees,
                agent,
                None,
                format!("{} vs {}", wired.display(), record.display()),
            ));
        }
    }
    findings
}

/// The canonical spelling, or the path as given when it cannot be resolved.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
