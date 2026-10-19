//! The capture-recovery leg. Report-only, it names a recovery hold in the
//! member's git dir and how long it has been held, because while one exists
//! every ordinary capture waits; it opens no lock and writes nothing. Under
//! `--fix` it runs one recovery job and releases its hold on every exit, so
//! ordinary capture never waits on more than this one run.
use std::path::Path;

use serde_json::json;

pub(super) fn doctor_capture_recovery(root: &Path, repair: bool) -> (String, serde_json::Value) {
    let Ok(repo) = rift_git::discover(root) else {
        return (String::new(), json!({"held": false}));
    };
    if repair {
        let words = fix::run(root, &repo).unwrap_or_else(|refusal| refusal.to_string());
        return (format!("{words}\n"), json!({"held": false, "fix": words}));
    }
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

#[cfg(unix)]
mod fix;

#[cfg(not(unix))]
mod fix {
    pub(super) fn run(
        _: &std::path::Path,
        _: &rift_git::RepoPaths,
    ) -> Result<String, &'static str> {
        Err("capture recovery runs on unix only")
    }
}
