//! The scratch checkout one client plays in, and the developer's own state
//! that must survive the run untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dogfood::measure::{parse_savings, SavingsSample};
use crate::dogfood::process::{capture, must};
use crate::dogfood::{Client, Lever};
use crate::dogfood_compare::Arm;

/// Which checkout one client's sessions play in: the client, the arm, and
/// the levers the line asked to write at `enforce`.
///
/// One context rather than three parameters, because the three travel
/// together from the run's loop to the preparation that writes the policy.
#[derive(Clone, Copy, Debug)]
pub(super) struct ArmOf<'a> {
    /// The client being driven.
    pub(super) client: Client,
    /// The arm the checkout plays; `None` is the single-arm run.
    pub(super) arm: Option<Arm>,
    /// The levers `--enforce` named, in order.
    pub(super) enforce: &'a [Lever],
}

/// What the preparation writes into a checkout's policy, if anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LeverWrites {
    /// Every read lever off: the calibration's removal.
    pub(super) off: bool,
    /// The levers written at `enforce`, in the order the line named them.
    pub(super) enforce: Vec<Lever>,
}

/// The policy writes an arm gets.
///
/// The off arm is the removal and carries no lever at `enforce` whatever the
/// line asked: an off arm with a lever on would measure two changes. Every
/// other arm — the on arm, the shell arm, the single-arm run — carries
/// exactly the levers named, and a run without the flag carries none, which
/// is the run as it was.
pub(super) fn lever_writes(arm: Option<Arm>, enforce: &[Lever]) -> LeverWrites {
    if arm == Some(Arm::Off) {
        return LeverWrites {
            off: true,
            enforce: Vec::new(),
        };
    }
    LeverWrites {
        off: false,
        enforce: enforce.to_vec(),
    }
}

/// The two documents the scripted sessions read, copied (never moved, never
/// edited) out of the converter's own corpus.
const DOCUMENTS: [(&str, &str); 2] = [
    (
        "crates/rift-recall-doc/fixtures/corpus/spec.docx",
        "docs/reference/spec.docx",
    ),
    (
        "crates/rift-recall-doc/fixtures/corpus/text-layer.pdf",
        "docs/reference/overview.pdf",
    ),
];

/// The two readings one preparation takes, and which question each answers.
///
/// They are two readings rather than one because the preparation does work of
/// its own between them: the shared-family refusal has to happen before
/// anything is enabled, while the priming of the documents — four `doc read`
/// calls — books rows of its own, and a session measured against the earlier
/// reading would carry that priming as if the client had done it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    /// The reading taken right after `init`, before `enable`.
    minted: SavingsSample,
    /// The reading taken at the end of the preparation, after the priming.
    primed: SavingsSample,
}

impl Prepared {
    /// Both readings, in the order the preparation took them.
    #[must_use]
    pub const fn new(minted: SavingsSample, primed: SavingsSample) -> Self {
        Self { minted, primed }
    }

    /// The reading the shared-family refusal is made against.
    ///
    /// It is the first one, taken while the checkout is still untouched: a
    /// clone that resolved to a family already being measured must be refused
    /// before the preparation writes a single row into it.
    #[must_use]
    pub const fn minted(&self) -> &SavingsSample {
        &self.minted
    }

    /// The reading a session's delta is measured against.
    ///
    /// It is the last one, so the first session's figures are the first
    /// session's own and not the preparation's priming as well.
    #[must_use]
    pub const fn baseline(&self) -> &SavingsSample {
        &self.primed
    }
}

/// A file copied aside so it can be put back exactly as it was.
pub(super) struct Untouched {
    /// The file that must end the run as it began.
    pub(super) path: PathBuf,
    /// The copy, made with its modification time preserved.
    backup: PathBuf,
}

impl Untouched {
    /// Copy `path` aside into `out`, timestamp and all.
    pub(super) fn snapshot(path: PathBuf, out: &Path) -> Option<Self> {
        let backup = out.join("codex-config.toml.bak");
        let copied = Command::new("/bin/cp")
            .arg("-p")
            .arg(&path)
            .arg(&backup)
            .status()
            .is_ok_and(|s| s.success());
        copied.then_some(Self { path, backup })
    }

    /// Put the file back if anything changed it. Returns whether it had to.
    pub(super) fn restore(&self) -> bool {
        let (Ok(now), Ok(then)) = (std::fs::read(&self.path), std::fs::read(&self.backup)) else {
            return false;
        };
        if now == then {
            return false;
        }
        let _ = std::fs::write(&self.path, &then);
        // `-r` copies the reference file's timestamps, so the file ends the
        // run with the modification time it started with as well as the bytes.
        let _ = Command::new("/usr/bin/touch")
            .arg("-r")
            .arg(&self.backup)
            .arg(&self.path)
            .status();
        true
    }
}

/// Lines across every window journal in a checkout.
#[must_use]
pub fn ledger_lines(scratch: &Path) -> u64 {
    let dir = scratch.join(".rift").join("state").join("reads");
    let mut total = 0u64;
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|x| x == "jsonl") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                total = total.saturating_add(text.lines().count() as u64);
            }
        }
    }
    total
}

/// Refuse a scratch family that is already being measured.
///
/// The developer's own family is the first thing in `seen`, because a reading
/// that wrote into somebody's real evaluation window would be worth nothing.
/// Every family this run has already minted follows it: with two arms the
/// whole question is whether the levers moved anything, and two arms sharing
/// one ledger would answer it by construction.
///
/// # Errors
/// A family whose capture or recall projection is one of `seen`.
pub fn refuse_shared_family(
    theirs: &SavingsSample,
    seen: &[(String, String)],
) -> Result<(), String> {
    for (recall_db, projection_db) in seen {
        if theirs.recall_db == *recall_db || theirs.projection_db == *projection_db {
            return Err(format!(
                "dogfood: the scratch checkout resolved to a family that is already \
                 being measured ({}) — refusing to measure into it",
                theirs.recall_db
            ));
        }
    }
    Ok(())
}

/// Prepare one client's scratch checkout: clone, documents, `init`, `enable`,
/// the release binary, and the assertion that this is not the developer's
/// family.
pub(super) fn prepare(
    root: &Path,
    rift: &Path,
    scratch: &Path,
    seen: &[(String, String)],
    of: ArmOf<'_>,
) -> Result<Prepared, String> {
    clone_with_documents(root, scratch)?;
    must(
        rift,
        &[
            "init".to_owned(),
            "--skip-splash".to_owned(),
            "--quiet".to_owned(),
        ],
        scratch,
    )?;
    // The family the scratch checkout minted must be its own — not the
    // developer's, and not the other arm's.
    let theirs = read_savings(rift, scratch)?;
    refuse_shared_family(&theirs, seen)?;
    wire_client(rift, of.client, scratch)?;
    prime_documents(rift, scratch);
    // Last, so that everything before it — the clone, the wiring, the primed
    // documents — is the same work in both arms and the levers are the only
    // thing that differs.
    // The shell arm keeps the overlay as the on arm has it: its one
    // difference from the on arm is how the client is driven, not what the
    // policy says.
    let writes = lever_writes(of.arm, of.enforce);
    if writes.off {
        let policy = crate::dogfood_compare::write_levers_off(scratch)?;
        println!("dogfood: every read lever off in {}", policy.display());
    }
    if !writes.enforce.is_empty() {
        let policy = crate::dogfood_compare::write_levers_enforced(scratch, &writes.enforce)?;
        println!(
            "dogfood: {} at enforce in {}",
            writes
                .enforce
                .iter()
                .map(|l| l.key())
                .collect::<Vec<_>>()
                .join(", "),
            policy.display()
        );
    }
    // The baseline, taken here rather than beside the refusal above: everything
    // the preparation books — the four primed `doc read` calls above most of
    // all — belongs to the preparation, not to the first session.
    let primed = read_savings(rift, scratch)?;
    Ok(Prepared::new(theirs, primed))
}

/// What `rift savings --json` reports for the family a checkout resolves to.
///
/// # Errors
/// The verb refused, or its output was not the document it should have been.
pub(super) fn read_savings(rift: &Path, scratch: &Path) -> Result<SavingsSample, String> {
    parse_savings(&must(
        rift,
        &["savings".to_owned(), "--json".to_owned()],
        scratch,
    )?)
}

/// Clone this checkout beside itself and copy in the two documents the
/// scripted sessions read.
///
/// The clone is `--shared`, so it costs a few megabytes rather than the whole
/// object store; the documents are copied rather than moved, because they
/// belong to the converter's own corpus.
fn clone_with_documents(root: &Path, scratch: &Path) -> Result<(), String> {
    must(
        Path::new("/usr/bin/git"),
        &[
            "clone".to_owned(),
            "--shared".to_owned(),
            "--quiet".to_owned(),
            root.display().to_string(),
            scratch.display().to_string(),
        ],
        root,
    )?;
    std::fs::create_dir_all(scratch.join("docs/reference"))
        .map_err(|e| format!("dogfood: docs/reference: {e}"))?;
    for (from, to) in DOCUMENTS {
        std::fs::copy(root.join(from), scratch.join(to))
            .map_err(|e| format!("dogfood: copy {from}: {e}"))?;
    }
    Ok(())
}

/// Wire the scratch checkout to one client, and point its hooks at the binary
/// this run drives rather than at whatever is on `PATH`.
fn wire_client(rift: &Path, client: Client, scratch: &Path) -> Result<(), String> {
    must(
        rift,
        &[
            "enable".to_owned(),
            "--agent".to_owned(),
            client.slug().to_owned(),
        ],
        scratch,
    )?;
    must(
        rift,
        &[
            "hooks".to_owned(),
            "install".to_owned(),
            "--repoint".to_owned(),
            "--binary".to_owned(),
            rift.display().to_string(),
        ],
        scratch,
    )?;
    Ok(())
}

/// Read both documents through the gate until their sidecars exist.
///
/// A document is converted in the background the first time it is read, and
/// that first read is past the gate's deadline: it journals the conversion and
/// runs untouched. So the priming here is the whole sequence a developer who
/// opened both files once would have driven — ask, drain the journal, ask
/// again — because a client that meets an unconverted document measures a race
/// rather than a lever.
fn prime_documents(rift: &Path, scratch: &Path) {
    let read_both = || {
        for (_, to) in DOCUMENTS {
            let _ = capture(
                rift,
                &["doc".to_owned(), "read".to_owned(), to.to_owned()],
                scratch,
            );
        }
    };
    read_both();
    let _ = capture(
        rift,
        &[
            "hooks".to_owned(),
            "worker".to_owned(),
            "--git-dir".to_owned(),
            scratch.join(".git").display().to_string(),
        ],
        scratch,
    );
    read_both();
    let sidecars = scratch.join(".rift").join("state").join("doc");
    let converted = std::fs::read_dir(&sidecars).into_iter().flatten().count();
    println!(
        "dogfood: {converted} document sidecar(s) ready in {}",
        sidecars.display()
    );
}

/// The developer's global Codex configuration.
///
/// Codex writes a `trust_level` row into this file for a project directory it
/// has run in — the harness never asks for that, and it is not the harness's
/// file to change. So it is copied aside with its timestamp before a client
/// runs and put back byte for byte afterwards, which is what the archived
/// verification of this behaviour did.
pub(super) fn codex_config() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".codex").join("config.toml"))
        .filter(|p| p.is_file())
}

/// Delete a client's scratch checkout and sweep the registry row it created.
pub(super) fn tidy(rift: &Path, root: &Path, scratch: &Path) {
    if std::fs::remove_dir_all(scratch).is_err() {
        println!(
            "dogfood: could not remove {} — remove it by hand",
            scratch.display()
        );
        return;
    }
    // The registry is append-only; `prune` appends one superseding removed
    // event per row whose path is gone, which is exactly what the scratch
    // checkout's row now is.
    match capture(rift, &["projects".to_owned(), "prune".to_owned()], root) {
        Ok((Some(0), _, _)) => {}
        _ => println!("dogfood: `rift projects prune` did not run — sweep the row by hand"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::SavingsDeltas;
    use crate::workspace_root;

    /// Neither arm may play in a family another arm — or the developer — is
    /// already measuring. Two arms sharing one ledger would answer the
    /// question the calibration exists to ask, by construction.
    #[test]
    fn the_two_arms_never_share_a_family() {
        let family = |tag: &str| SavingsSample {
            recall_db: format!("/scratch/{tag}/recall.db"),
            projection_db: format!("/scratch/{tag}/projection.db"),
            ..SavingsSample::default()
        };
        let mine = family("mine");
        let off = family("off");
        let on = family("on");
        let seen = |s: &[&SavingsSample]| -> Vec<(String, String)> {
            s.iter()
                .map(|v| (v.recall_db.clone(), v.projection_db.clone()))
                .collect()
        };
        // The off arm against the developer's family, then the on arm against
        // both — the order the run checks them in.
        assert!(refuse_shared_family(&off, &seen(&[&mine])).is_ok());
        assert!(refuse_shared_family(&on, &seen(&[&mine, &off])).is_ok());
        // The same recall ledger twice.
        let err = refuse_shared_family(&off, &seen(&[&mine, &off])).unwrap_err();
        assert!(err.contains("already being measured"), "{err}");
        assert!(err.contains("/scratch/off/recall.db"), "{err}");
        // A shared capture projection is the same refusal: either half is one
        // family's durable state.
        let half = SavingsSample {
            recall_db: "/scratch/elsewhere/recall.db".to_owned(),
            projection_db: mine.projection_db.clone(),
            ..SavingsSample::default()
        };
        assert!(refuse_shared_family(&half, &seen(&[&mine])).is_err());
        // And with nothing measured yet, nothing is refused.
        assert!(refuse_shared_family(&off, &[]).is_ok());
    }
    /// The preparation primes the two documents by hand, and that priming
    /// books rows of its own. The baseline a session is measured against is
    /// therefore the reading taken *after* it, so the first session's delta is
    /// the first session's own work and not four `doc read` calls the harness
    /// made.
    #[test]
    fn the_baseline_is_sampled_after_the_priming() {
        // A fresh family: `init` has run and nothing has been booked.
        let minted = SavingsSample {
            recall_db: "/scratch/recall.db".to_owned(),
            projection_db: "/scratch/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        // The same family at the end of the preparation, carrying the shape a
        // live run reported for the priming alone: four document decisions.
        let primed = SavingsSample {
            tokens_avoided: 718,
            tokens_returned: 494,
            ..minted.clone()
        };
        let prepared = Prepared::new(minted.clone(), primed.clone());
        assert_eq!(prepared.baseline(), &primed, "the later reading");
        assert_eq!(prepared.minted(), &minted, "and the earlier one is kept");
        // What the family reads after the first session: the priming, plus the
        // session's own work.
        let after_one = SavingsSample {
            tokens_avoided: 718 + 240,
            tokens_returned: 494 + 130,
            read_tokens: 3_000,
            ..minted.clone()
        };
        let d = SavingsDeltas::between(prepared.baseline(), &after_one);
        assert_eq!(d.tokens_avoided, 240, "the session's own avoided tokens");
        assert_eq!(d.tokens_returned, 130, "and its own returned tokens");
        assert_eq!(d.read_tokens, 3_000);
        // Measured against the earlier reading it would absorb the priming,
        // which is the reading this baseline exists to avoid.
        let absorbed = SavingsDeltas::between(prepared.minted(), &after_one);
        assert_eq!(absorbed.tokens_avoided, 958);
        assert_eq!(absorbed.tokens_returned, 624);
    }
    /// The shared-family refusal keeps reading the first sample: it is taken
    /// while the checkout is still untouched, and a clone that resolved to a
    /// family already being measured has to be refused before the preparation
    /// enables anything or writes a row.
    #[test]
    fn the_refusal_still_reads_the_first_sample() {
        let minted = SavingsSample {
            recall_db: "/theirs/recall.db".to_owned(),
            projection_db: "/theirs/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        // Distinguishable only so this test can say which reading was
        // consulted; one preparation's two readings name one family.
        let primed = SavingsSample {
            recall_db: "/elsewhere/recall.db".to_owned(),
            projection_db: "/elsewhere/projection.db".to_owned(),
            ..SavingsSample::default()
        };
        let prepared = Prepared::new(minted, primed);
        let seen = vec![(
            "/theirs/recall.db".to_owned(),
            "/theirs/projection.db".to_owned(),
        )];
        let refused = refuse_shared_family(prepared.minted(), &seen);
        assert!(refused.is_err(), "the family the refusal was made against");
        assert!(
            refused.unwrap_err().contains("/theirs/recall.db"),
            "and it names that family"
        );
        assert!(
            refuse_shared_family(prepared.baseline(), &seen).is_ok(),
            "the later reading names another family — refusing on it would let \
             a shared family through"
        );
    }
    /// What a checkout's policy is written with: the off arm goes off and
    /// never carries a lever at enforce, every other arm carries exactly the
    /// levers the line named, and without the flag nothing is written at all.
    #[test]
    fn without_the_flag_no_lever_is_written_and_the_off_arm_stays_off() {
        assert_eq!(
            lever_writes(None, &[]),
            LeverWrites::default(),
            "today's run: nothing written"
        );
        assert_eq!(lever_writes(Some(Arm::On), &[]), LeverWrites::default());
        assert_eq!(
            lever_writes(None, &[Lever::Narrow]),
            LeverWrites {
                off: false,
                enforce: vec![Lever::Narrow]
            }
        );
        assert_eq!(
            lever_writes(Some(Arm::Shell), &[Lever::FirstContact, Lever::Narrow]),
            LeverWrites {
                off: false,
                enforce: vec![Lever::FirstContact, Lever::Narrow]
            }
        );
        assert_eq!(
            lever_writes(Some(Arm::Off), &[Lever::Narrow]),
            LeverWrites {
                off: true,
                enforce: Vec::new()
            },
            "the off arm is the removal, whatever the line asks to enforce"
        );
    }

    /// The documents the scripted sessions read are copied out of the
    /// converter's own corpus, and both are still there.
    #[test]
    fn the_documents_the_sessions_read_still_ship() {
        let root = workspace_root();
        for (from, to) in DOCUMENTS {
            assert!(root.join(from).is_file(), "{from} is gone");
            assert!(to.starts_with("docs/"), "{to} lands in the checkout");
        }
    }
}
