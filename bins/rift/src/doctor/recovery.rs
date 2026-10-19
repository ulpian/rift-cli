//! The capture-recovery leg: report-only, it names a recovery hold in the
//! member's git dir and how long it has been held, because while one exists
//! every ordinary capture waits. It opens no lock and writes nothing.
use std::path::Path;

use serde_json::json;

pub(super) fn doctor_capture_recovery(root: &Path) -> (String, serde_json::Value) {
    let Ok(repo) = rift_git::discover(root) else {
        return (String::new(), json!({"held": false}));
    };
    match held_words(&repo.git_dir) {
        Some(words) => (format!("{words}\n"), json!({"held": true, "words": words})),
        None => (String::new(), json!({"held": false})),
    }
}

#[cfg(unix)]
fn held_words(git_dir: &Path) -> Option<String> {
    crate::capture_recovery::job::held_words(git_dir, std::time::SystemTime::now())
}

#[cfg(not(unix))]
fn held_words(_git_dir: &Path) -> Option<String> {
    None
}
