//! The property the inclusion control is not allowed to weaken.
//!
//! Opting a git-ignored file into capture widens what Rift *takes*. It must
//! never widen what reaches durable storage unredacted, and the two are easy to
//! conflate: the opted-in files are exactly the ones a developer keeps out of
//! git *because* they hold credentials — `.env`, a local settings file, a
//! machine-specific token. So the interesting input is not a well-behaved text
//! file, it is a secret, and the assertion is that the gate sees it.
//!
//! This drives the same two functions the condense path composes —
//! `inclusion::capture_document` into `rift_shadow_redact::redact` — rather than
//! a stand-in, so a change to either that broke the pairing fails here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_shadow::inclusion::{capture_document, CAPTURE_DOC_HEADER, INCLUDE_FILE};

use proptest::prelude::*;

/// Does `haystack` contain `needle` anywhere?
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The id the shard would be written under for this working tree — the same
/// function `prepare-commit-msg` predicts with and `condense` derives, so an
/// equality here is an equality of durable identity, not of an intermediate.
///
/// Serves only the symlinked-control-file tests below, which are `#[cfg(unix)]`
/// because creating a symlink needs a privilege on Windows; gated the same
/// way so it is not dead code there.
#[cfg(unix)]
fn shard_id(work_tree: &std::path::Path) -> String {
    rift_shadow::predicted_checkpoint_id(
        "session-under-test",
        &capture_document(work_tree),
        &[],
        &rift_shadow::redact::RulePackSet::builtin(),
    )
    .unwrap()
}

/// A repository can ship its control file as a **symlink** — git stores mode
/// `120000` and clone recreates it — so `.worktreeinclude` may point at
/// `~/.aws/credentials` or into `.git`. Read through, every line of the target
/// becomes an include entry, and an entry that names nothing in the tree is
/// echoed verbatim into the durable document as `x <line> missing`: a file the
/// developer can read, laundered into a checkpoint, gated only by whether the
/// redactor's detectors happen to recognise its lines.
///
/// The assertion is the strongest observable one: not "the secret is absent"
/// but "this tree is the no-capture tree" — byte-identical empty bytes AND the
/// same checkpoint id a repository with no control file mints, which is what
/// keeps the silent-empty ruling free of any id-visible side effect.
#[cfg(unix)]
#[test]
fn a_symlinked_control_file_lands_on_the_no_capture_checkpoint_id() {
    for (name, secret) in [
        (
            "credentials",
            "aws_secret_access_key = SECRET-BEYOND-THE-LINK",
        ),
        (
            "gitconfig",
            "\turl = https://x:GIT-TOKEN-BEYOND-THE-LINK@h/r",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join(name);
        std::fs::write(&outside, format!("{secret}\n")).unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&outside, work.join(INCLUDE_FILE)).unwrap();

        // The durable identity first, because it is the assertion that would
        // still hold someone honest if the bytes ever changed shape: a bare tree
        // with no control file at all mints exactly this id.
        let bare = tempfile::tempdir().unwrap();
        assert_eq!(
            shard_id(&work),
            shard_id(bare.path()),
            "a symlinked control file must not move the shard id ({name})",
        );

        let raw = capture_document(&work);
        assert!(
            !contains(raw.as_slice(), secret.as_bytes()),
            "the control file was followed and its target's bytes came back",
        );
        assert!(raw.as_slice().is_empty(), "and the capture is empty");
    }
}

proptest! {
    /// An opted-in ignored file's secret is captured locally and masked
    /// durably. Both halves matter: without the first assertion the test could
    /// pass by capturing nothing at all, which is exactly the failure mode a
    /// "no secret in the output" check invites.
    #[test]
    fn an_included_files_secret_is_captured_locally_and_masked_durably(
        key in "[0-9A-Z]{16}",
        lead in "[a-z_]{0,12}",
        tail in "[a-z ]{0,12}",
    ) {
        let secret = format!("AKIA{key}");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            format!("# {lead}\nAWS_ACCESS_KEY_ID={secret}\n{tail}\n"),
        ).unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), ".env\n").unwrap();

        // Local, raw, in-memory: the secret IS here. `SecretBytes` withholds
        // `Debug` precisely so this stage cannot reach a log line.
        let raw = capture_document(dir.path());
        prop_assert!(
            contains(raw.as_slice(), secret.as_bytes()),
            "the local capture really did take the file's bytes",
        );

        // The one-way gate — the same call `condense` makes, no stand-in.
        let out = rift_shadow_redact::redact(&raw).unwrap();
        prop_assert!(
            !contains(out.redacted.as_slice(), secret.as_bytes()),
            "the secret must not survive into durable bytes",
        );
        prop_assert!(out.report.total_spans >= 1, "and the report counted it");

        // The framing survives redaction: masking substitutes fixed
        // placeholders and never introduces a newline, so the record structure
        // that says WHICH file this was is still readable beside the mask.
        let text = String::from_utf8(out.redacted.as_slice().to_vec()).unwrap();
        prop_assert!(text.starts_with(CAPTURE_DOC_HEADER));
        prop_assert!(text.contains("\nf .env\n"));
        prop_assert!(text.contains("[REDACTED:"));
    }

    /// The refusals carry no bytes to leak. A `.git` path, a symlink and an
    /// oversize file each produce a record naming the path and the reason and
    /// nothing else — so the document cannot become a side channel for the
    /// content it just declined to capture.
    #[test]
    fn a_refused_path_contributes_no_content(body in "[A-Za-z0-9]{32,64}") {
        let dir = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(dir.path()).unwrap();
        std::fs::write(dir.path().join(".git/config"), &body).unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), ".git/config\n").unwrap();

        let raw = capture_document(dir.path());
        prop_assert!(!contains(raw.as_slice(), body.as_bytes()));
        prop_assert_eq!(
            String::from_utf8(raw.as_slice().to_vec()).unwrap(),
            format!("{CAPTURE_DOC_HEADER}\nx .git/config git-path\n"),
        );
    }
}
