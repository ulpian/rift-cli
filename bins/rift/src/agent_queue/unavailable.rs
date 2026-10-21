//! The record a basis-snapshot job leaves when it completed `unavailable`,
//! apart from the journal so the journal's own file stays under its ceiling.

use std::path::Path;

use super::names::{self, Marker, QueueName};
use super::{logical_stem, queue_dir, AgentJob};

/// The marker a basis-snapshot job leaves when it completed `unavailable`:
/// keyed by the job's logical stem, so the same revision found gone twice
/// counts once. Best-effort, like every marker here: a count `rift doctor`
/// reports, never something a decision reads.
pub(crate) fn record_unavailable(git_dir: &Path, job: &AgentJob) {
    let Some(stem) = logical_stem(job) else {
        return;
    };
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(names::unavailable(&stem)), b"");
    }
}

/// How many basis-snapshot jobs completed `unavailable`.
pub(crate) fn unavailable_count(git_dir: &Path) -> usize {
    rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| names::classify(name) == QueueName::Marker(Marker::Unavailable))
        })
        .count()
}
