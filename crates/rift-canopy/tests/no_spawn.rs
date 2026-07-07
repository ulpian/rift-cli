//! The structural no-spawn proof (M5-07/08, PRD user story 12): no process
//! spawn is reachable from the palette's parse or dispatch — the free-text
//! surface is inert by construction. The whole dashboard crate is scanned:
//! rift-canopy renders and reduces; it never executes. The ONLY door to the
//! CLI surface is the injected in-process `CommandDispatcher`, whose
//! implementations live in the binary (outside this crate) and receive typed
//! requests, never command strings.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

/// Source tokens that would mean a process can be born here.
const FORBIDDEN: &[&str] = &[
    "std::process::Command",
    "process::Command::new",
    "Command::new(",
    ".spawn(",
    "std::process::exit",
    "libc::exec",
];

fn scan(dir: &Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, hits);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for token in FORBIDDEN {
            for (i, line) in text.lines().enumerate() {
                // Comments and doc lines may NAME the tokens (this contract
                // is documented in prose); code may not.
                let code = line.split("//").next().unwrap_or("");
                if code.contains(token) {
                    hits.push(format!("{}:{}: {token}", path.display(), i + 1));
                }
            }
        }
    }
}

#[test]
fn no_process_spawn_is_reachable_from_the_dashboard_crate() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "rift-canopy must never be able to spawn a process; found:\n{}",
        hits.join("\n")
    );
}

/// The `hooks` wire entry is structurally absent from the grammar: the word
/// list cannot advertise it and the parser cannot produce it.
#[test]
fn hooks_is_unparseable_and_unlisted() {
    assert!(!rift_canopy::VERB_WORDS.contains(&"hooks"));
    assert!(rift_canopy::parse("/hooks git post-commit").is_err());
    assert!(rift_canopy::parse("hooks claude-code SessionStart").is_err());
}
