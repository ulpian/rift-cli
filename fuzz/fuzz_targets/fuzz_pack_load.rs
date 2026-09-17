#![no_main]
//! Fuzz target for the **rule-pack path**.
//!
//! The redaction fuzzing bar names the pack path among the untrusted-byte
//! surfaces it covers, so the path that reads *user-authored policy files off
//! disk* owes a target the way the walker does. The fuzzer's bytes are spent on
//! **pack files**, not on content: the script writes a directory tree under a
//! temporary `.rift/policies/redactors/`, hands it to the shipped loader, feeds
//! what loaded into the shipped compiler, and redacts a planted-secret corpus
//! with the result.
//!
//! ```text
//! bytes → pack files on disk → load_redactor_packs → PackSpec → with_pack → redact
//! ```
//!
//! **The contract, in four parts:**
//!
//! 1. **Never panics.** Every step is reached with attacker-shaped bytes: file
//!    names, directory nesting, TOML/JSON syntax, regex source, rule ids and
//!    sample text all come from the script, and one body arm splices the raw
//!    input verbatim so the decoders are fuzzed as decoders and not only through
//!    templates.
//! 2. **A diagnostic never becomes an error.** This is the property the capture
//!    path lives on — a policy file someone is mid-edit must never fail a commit
//!    — and that property is explicitly a property of the *composition*, not of
//!    the loader alone. [`rift_config::load_redactor_packs`] is infallible by
//!    signature, so asserting on it says nothing; what can still go wrong is the
//!    step after it, where a pattern is compiled and self-tested. So the target
//!    performs the same composition `bins/rift`'s `capture_rule_packs` performs
//!    and asserts the whole of it yields a usable pipeline. Stated plainly
//!    because it is a limitation: `capture_rule_packs` is `pub(crate)` in a
//!    lib-less binary and cannot be called from here, so this target binds the
//!    composition's *shape*, and the **shipped** composition is bound end to end
//!    by `bins/rift/tests/redactor_pack_e2e.rs`, which drives the real binary
//!    through a real hook. Both are needed; neither substitutes for the other.
//! 3. **The caps hold** — 256 files, 1 MiB per file, depth 8, 64-character
//!    identifiers — *including when the files are malformed*, which is the arm
//!    that was once wrong in this loader: the file cap counted packs that
//!    parsed, so a directory of garbage was read in full and the guard never
//!    fired. [`Scenario::CapTrip`] is a directory over the cap made fully of
//!    files that do not parse.
//! 4. **No planted secret survives, and the output is stable under
//!    re-redaction.** A pack can only ever *add* coverage, so whatever the
//!    script wrote, the built-in pipeline's guarantees have to be intact after
//!    it — a hostile pack must not be able to take a built-in detector away.
//!
//! Modelled on `redact_jsonl.rs`, which reads the fuzzer's bytes as a script
//! that builds a structured input rather than as the input itself.
//!
//! **The first byte picks the scenario** (`data[0] % 5`, see [`Scenario`]), and
//! that is a deliberate interface rather than an implementation detail: it is
//! what lets a committed seed *provably* reach the arm it is named for, instead
//! of a seed that reached it on the tree it was recorded on. The committed
//! corpus is one seed per arm, named for it:
//!
//! | seed | first byte | arm |
//! |---|---|---|
//! | `seed-valid` | `0x00` | [`Scenario::Valid`] |
//! | `seed-malformed` | `0x01` | [`Scenario::Malformed`] |
//! | `seed-duplicate` | `0x02` | [`Scenario::Duplicate`] |
//! | `seed-yaml` | `0x03` | [`Scenario::Yaml`] |
//! | `seed-cap-trip` | `0x04` | [`Scenario::CapTrip`] |

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;
use rift_core::SecretBytes;
use rift_shadow_redact::{PackRule, PackSample, PackSpec, RulePackSet};

/// The loader's caps. Spelled here because they are private to `rift-config`;
/// each is pinned there by its own unit test, and this target is the statement
/// that the *composition* still honours them with attacker-written files.
const MAX_PACK_FILES: usize = 256;
const MAX_IDENTIFIER_CHARS: usize = 64;
const MAX_PACK_FILE_BYTES: usize = 1 << 20;
const MAX_DEPTH: usize = 8;

/// Secrets the **built-in** pipeline seals with no pack's help. Whatever the
/// script writes, these must still go: a pack is additive, and no pack may cost
/// the repository coverage it already had.
const SECRETS: &[&str] = &[
    "AKIAIOSFODNN7EXAMPLE",
    "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789",
    "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q",
    "alice@example.com",
];

/// Regex source the script can put in a rule. Several of these are patterns the
/// compiler must refuse *as one rule* without taking the pipeline down: one that
/// will not compile, one that matches the empty string, one that matches
/// everything.
const PATTERNS: &[&str] = &[
    "ACME-[A-Z0-9]{8}",
    "x*",
    "(",
    "[",
    "a{2,1}",
    "(?i)internal-[0-9]+",
    ".*",
    "",
];

/// Rule ids, including one over the identifier cap and two the loader must
/// refuse as identifiers at all.
const RULE_IDS: &[&str] = &[
    "internal_token",
    "second",
    "email",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "a b",
    "",
];

const FILLER: &[&str] = &["hello world", "ok", "the quick brown fox"];

/// What the fuzzer's first byte selects. Each arm is a shape the pack path has
/// to survive, and each has a committed seed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// Well-formed packs at assorted depths.
    Valid,
    /// Files that do not parse: truncated, wrong stem, unknown key, raw bytes.
    Malformed,
    /// The same stem in two directories, so two packs claim one name — and
    /// repeated rule ids inside one pack, so one detector id is minted twice.
    Duplicate,
    /// `.yaml` / `.yml`, which this build reports as unsupported rather than
    /// silently ignoring (the workspace pins no YAML parser).
    Yaml,
    /// More candidate files than the cap, none of which parse.
    CapTrip,
}

impl Scenario {
    fn of(byte: u8) -> Self {
        match byte % 5 {
            0 => Self::Valid,
            1 => Self::Malformed,
            2 => Self::Duplicate,
            3 => Self::Yaml,
            _ => Self::CapTrip,
        }
    }
}

/// The fuzzer's bytes, read as a script rather than as content.
struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Script<'a> {
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        b
    }

    fn pick<T: Copy>(&mut self, choices: &[T]) -> T {
        let index = usize::from(self.byte()) % choices.len();
        choices[index]
    }

    /// Whatever is left, verbatim — the arm that fuzzes the TOML and JSON
    /// decoders as decoders. Never valid UTF-8 by construction, which is itself
    /// a path the loader has to report rather than fail on.
    fn rest(&mut self) -> &'a [u8] {
        let from = self.at.min(self.data.len());
        self.at = self.data.len();
        &self.data[from..]
    }

    fn spent(&self) -> bool {
        self.at >= self.data.len()
    }
}

/// A well-formed pack body under `name`, with script-chosen rules.
fn pack_body(script: &mut Script, name: &str, repeat_ids: bool) -> String {
    let rules = 1 + usize::from(script.byte() % 3);
    let mut out = format!("schema_version = 1\nname = \"{name}\"\nversion = \"2026.08.0\"\n");
    let first = script.pick(RULE_IDS);
    for _ in 0..rules {
        let id = if repeat_ids { first } else { script.pick(RULE_IDS) };
        let regex = script.pick(PATTERNS);
        out.push_str(&format!("\n[[rules]]\nid = \"{id}\"\nregex = \"{regex}\"\n"));
        if script.byte() % 3 == 0 {
            let input = script.pick(FILLER);
            let expected = script.byte() % 2 == 0;
            out.push_str(&format!(
                "\n[[rules.samples]]\ninput = \"{input}\"\nredacted = {expected}\n"
            ));
        }
    }
    out
}

/// A body that does not parse, four ways.
fn malformed_body(script: &mut Script, stem: &str) -> Vec<u8> {
    match script.byte() % 4 {
        0 => format!("schema_version = 1\nname = \"{stem}\"\n").into_bytes(),
        1 => pack_body(script, "wrongstem", false).into_bytes(),
        2 => format!("{}\nextra_key = true\n", pack_body(script, stem, false)).into_bytes(),
        _ => script.rest().to_vec(),
    }
}

/// `root/d0/d1/…` at `depth`, created. `None` when the filesystem refuses.
fn dir_at(root: &Path, depth: usize) -> Option<PathBuf> {
    let mut dir = root.to_path_buf();
    for d in 0..depth {
        dir.push(format!("d{d}"));
    }
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Write the tree the loader will walk. Returns how many *candidate* files
/// (`.toml`/`.json`) were written, which the cap assertion needs.
fn write_tree(script: &mut Script, scenario: Scenario, root: &Path) -> usize {
    let mut candidates = 0;
    let mut put = |path: &Path, bytes: &[u8], counts: bool| {
        if std::fs::write(path, bytes).is_ok() && counts {
            candidates += 1;
        }
    };
    match scenario {
        Scenario::CapTrip => {
            let Some(dir) = dir_at(root, 0) else { return 0 };
            for i in 0..MAX_PACK_FILES + 4 {
                put(&dir.join(format!("p{i}.toml")), b"not a pack at all\n", true);
            }
        }
        Scenario::Yaml => {
            let depth = usize::from(script.byte()) % (MAX_DEPTH + 3);
            let Some(dir) = dir_at(root, depth) else {
                return 0;
            };
            put(&dir.join("team.yaml"), b"name: team\n", false);
            put(&dir.join("other.yml"), script.rest(), false);
        }
        Scenario::Duplicate => {
            // One stem, two directories: `parse_pack` requires name == stem, so
            // this is the only way two packs can claim one name.
            let repeat_ids = script.byte() % 2 == 0;
            let body = pack_body(script, "dup", repeat_ids);
            for sub in ["a", "b"] {
                let dir = root.join(sub);
                if std::fs::create_dir_all(&dir).is_ok() {
                    put(&dir.join("dup.toml"), body.as_bytes(), true);
                }
            }
        }
        Scenario::Malformed => {
            let count = 1 + usize::from(script.byte() % 4);
            for i in 0..count {
                if script.spent() && i > 0 {
                    break;
                }
                let depth = usize::from(script.byte()) % (MAX_DEPTH + 3);
                let Some(dir) = dir_at(root, depth) else { continue };
                let stem = format!("p{i}");
                let body = malformed_body(script, &stem);
                let ext = if script.byte() % 2 == 0 { "toml" } else { "json" };
                put(&dir.join(format!("{stem}.{ext}")), &body, true);
            }
        }
        Scenario::Valid => {
            let count = 1 + usize::from(script.byte() % 3);
            for i in 0..count {
                let depth = usize::from(script.byte()) % (MAX_DEPTH + 3);
                let Some(dir) = dir_at(root, depth) else { continue };
                let stem = format!("p{i}");
                // Rarely, a file over the per-file byte cap.
                let body = if script.byte() % 31 == 0 {
                    "x".repeat(MAX_PACK_FILE_BYTES + 1)
                } else {
                    pack_body(script, &stem, false)
                };
                put(&dir.join(format!("{stem}.toml")), body.as_bytes(), true);
            }
        }
    }
    candidates
}

/// A per-process scratch root, wiped at the top of every iteration so one
/// iteration never inherits another's tree.
fn scratch() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("rift-fuzz-pack-{}-{}", std::process::id(), n % 4));
    p
}

fuzz_target!(|data: &[u8]| {
    let Some((&first, tail)) = data.split_first() else {
        return;
    };
    let scenario = Scenario::of(first);
    let root = scratch();
    let _ = std::fs::remove_dir_all(&root);
    // The pack directory is resolved by the SHIPPED resolver, so this target
    // cannot drift from the layout the product reads.
    let dir = rift_config::redactor_pack_dir(&root.join(".rift"));
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }

    let mut script = Script { data: tail, at: 0 };
    let candidates = write_tree(&mut script, scenario, &dir);

    // (1)+(2) Infallible by construction: a missing directory, a malformed file,
    // a bad regex, a colliding id and a failing self-test are all *data*.
    let loaded = rift_config::load_redactor_packs(&dir);

    // (3) The caps. A candidate past the cap is never read, so the packs that
    // loaded can never outnumber it however many files were written.
    assert!(
        loaded.packs.len() <= MAX_PACK_FILES,
        "the file cap did not hold: {} packs from {candidates} candidate files",
        loaded.packs.len()
    );
    if candidates > MAX_PACK_FILES {
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|d| d.reason.contains("file cap")),
            "{candidates} candidates were written past the {MAX_PACK_FILES}-file cap and no \
             diagnostic said so: {:?}",
            loaded.diagnostics
        );
    }
    for pack in &loaded.packs {
        assert!(
            pack.name.chars().count() <= MAX_IDENTIFIER_CHARS,
            "a pack name over the identifier cap loaded: {}",
            pack.name.chars().count()
        );
        for rule in &pack.rules {
            assert!(
                rule.id.chars().count() <= MAX_IDENTIFIER_CHARS,
                "a rule id over the identifier cap loaded: {}",
                rule.id.chars().count()
            );
        }
        // The file cap is on candidates, but a pack that loaded was also read,
        // so it was under the byte cap.
        if let Ok(meta) = std::fs::metadata(&pack.source) {
            assert!(
                meta.len() as usize <= MAX_PACK_FILE_BYTES,
                "a pack over the {MAX_PACK_FILE_BYTES}-byte cap loaded: {}",
                meta.len()
            );
        }
    }
    // A diagnostic carries a path and a reason and never the file's contents.
    for d in &loaded.diagnostics {
        assert!(!d.reason.is_empty(), "a diagnostic with no reason");
    }
    if scenario == Scenario::Yaml {
        assert!(
            loaded.packs.is_empty(),
            "a yaml file produced a pack in a build with no yaml parser"
        );
    }

    // The composition `bins/rift::capture_rule_packs` performs. It returns a
    // pipeline and a note list; there is no error arm, and that is the point.
    let mut set = RulePackSet::builtin();
    for pack in &loaded.packs {
        let spec = PackSpec {
            name: pack.name.clone(),
            version: pack.version.clone(),
            rules: pack
                .rules
                .iter()
                .map(|r| PackRule {
                    id: r.id.clone(),
                    regex: r.regex.clone(),
                    samples: r
                        .samples
                        .iter()
                        .map(|s| PackSample {
                            input: s.input.clone(),
                            redacted: s.redacted,
                        })
                        .collect(),
                })
                .collect(),
        };
        let (next, _warnings) = set.with_pack(&spec);
        set = next;
    }

    // (4) Whatever the script wrote, the built-in guarantees stand.
    let document = format!(
        "{} {}\n{{\"ssn\":\"123456789\"}}\n{} {}",
        FILLER[usize::from(first) % FILLER.len()],
        SECRETS[usize::from(script.byte()) % SECRETS.len()],
        SECRETS[usize::from(script.byte()) % SECRETS.len()],
        FILLER[usize::from(script.byte()) % FILLER.len()],
    );
    let Ok(output) = rift_shadow_redact::redact_with(&SecretBytes::from(document.as_str()), &set)
    else {
        panic!("a rule pack made redaction fail, which the capture path cannot survive");
    };
    let text = String::from_utf8_lossy(output.redacted.as_slice()).to_string();
    for secret in SECRETS {
        if document.contains(secret) {
            assert!(
                !text.contains(secret),
                "a planted secret survived a pack-loaded pipeline\n  secret: {secret}\n  \
                 output: {text}"
            );
        }
    }
    assert!(
        !text.contains("123456789"),
        "the labelled identifier survived a pack-loaded pipeline: {text}"
    );

    // Stable on its own output, with the packs still in the set.
    let Ok(second) = rift_shadow_redact::redact_with(&SecretBytes::from(text.as_str()), &set)
    else {
        panic!("re-redaction failed on a pipeline's own output");
    };
    assert_eq!(
        second.redacted.as_slice(),
        text.as_bytes(),
        "output was not stable under re-redaction"
    );

    let _ = std::fs::remove_dir_all(&root);
});
