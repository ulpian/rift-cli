//! Where the policy came from: the one path decision over `.rift/config.toml`
//! (applied, absent, or refused and why) and the checkpoint-remote resolver
//! that reads the same file under the same guard.
//!
//! Kept apart so every reader of the policy file shares one trust decision.

use std::path::Path;

use rift_core::{Result, RiftError};

/// Why a policy file that is present was not applied. Each variant names what
/// [`std::fs::symlink_metadata`] actually saw, because that is the whole of what
/// this guard can know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// `.rift` is not a directory — a symlink into somewhere else on the
    /// machine, or a plain file standing in its place.
    RiftDirIsNotADirectory,
    /// `.rift/config.toml` is a symlink, so reading it would read a file the
    /// work tree does not own.
    ConfigIsASymlink,
    /// `.rift/config.toml` is neither a regular file nor a symlink — a
    /// directory, a fifo, a device.
    ConfigIsNotARegularFile,
}

/// Where the effective policy came from.
///
/// The load path degrades to secure defaults whenever it cannot trust the file,
/// which is the right posture and was, until this existed, **silent**. What
/// degrading loses is not the built-in floor — that always holds, and a refused
/// load produces exactly the same `sensitive_paths` a default load does — it is
/// the *developer's declared* policy: their extra `sensitive_paths`, their
/// model allow/deny lists, `raw_transcripts`, the retention class, the
/// checkpoint strategy, the entropy threshold. A project can therefore be
/// running with none of the policy it wrote and no way to tell.
///
/// This is the answer, carried on the config itself so a caller cannot forget to
/// ask for it. Deliberately **not** part of `policy_hash`: the hash identifies
/// the policy's content, and two projects with identical policy must hash the
/// same whether one of them reached it by refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    /// The project's declared policy was read and applied.
    Declared,
    /// No `.rift/config.toml` is present; the built-in secure defaults apply and
    /// nothing was lost.
    Default,
    /// A policy file is present and was **refused**. The built-in floor is in
    /// force; everything the project declared is not.
    Refused(PolicyRefusal),
}

impl PolicySource {
    /// A one-line diagnostic when the declared policy did not apply, so the
    /// surface that reports it does not re-derive the reasoning.
    #[must_use]
    pub fn diagnostic(self) -> Option<&'static str> {
        match self {
            Self::Declared | Self::Default => None,
            Self::Refused(PolicyRefusal::RiftDirIsNotADirectory) => Some(
                ".rift is not a directory this work tree owns (a symlink or a file stands in \
                 its place), so .rift/config.toml was not read: built-in secure defaults are \
                 in force and the project's declared policy is not applied",
            ),
            Self::Refused(PolicyRefusal::ConfigIsASymlink) => Some(
                ".rift/config.toml is a symlink, so it was not read: built-in secure defaults \
                 are in force and the project's declared policy is not applied",
            ),
            Self::Refused(PolicyRefusal::ConfigIsNotARegularFile) => Some(
                ".rift/config.toml is not a regular file, so it was not read: built-in secure \
                 defaults are in force and the project's declared policy is not applied",
            ),
        }
    }

    /// Whether the project's declared policy is actually in force.
    #[must_use]
    pub fn is_declared_policy_applied(self) -> bool {
        matches!(self, Self::Declared)
    }
}

/// The single path decision this crate makes about its policy file: was it
/// applied, is there none, or was it refused and why.
///
/// Both `.rift` and `.rift/config.toml` are repository-shippable names, and a
/// symlink survives a clone (git stores it as mode 120000). `Path::exists` and
/// `read_to_string` both *follow*, so a repository could point its policy file
/// at `~/.aws/credentials` and have this crate read it — and a TOML parse
/// error's `Display` quotes the offending source line, which puts a line of the
/// target on whichever terminal prints the error. Measured, before this guard:
/// `config error: …/config.toml: TOML parse error at line 2, column 21` followed
/// by the credentials line itself. A linked `.rift` **directory** is the worse
/// half: a valid-TOML target parses, so the policy is not echoed but silently
/// *loaded* from outside the work tree.
///
/// `symlink_metadata` reports the link rather than its target, so a linked
/// directory or a linked file is refused and the caller degrades to secure
/// defaults — policy that cannot be trusted is policy that does not apply, never
/// policy that leaks. A directory in the file's place is refused too (it used to
/// reach `read_to_string` and surface an IO error instead).
///
/// Absent and refused are deliberately different answers. Both degrade to the
/// same secure defaults, but only one of them means a developer wrote policy
/// that is not running — see [`PolicySource`].
///
/// Two residuals, both outside what a repository can ship and both stated
/// because this doc's framing is broader than the guard:
///
/// * a **TOCTOU race** between this check and the read, which std cannot close
///   (`O_NOFOLLOW` needs a libc `OpenOptions` flag);
/// * a **hard link**. `symlink_metadata` reports a hard link to
///   `~/.aws/credentials` as an ordinary regular file, because on the filesystem
///   it *is* one — there is no link to see. So the file is read and a TOML parse
///   error still quotes its line. Git cannot represent a hard link (it stores
///   blobs, and a clone writes fresh files), so this is not repo-shippable and
///   is outside the threat model; making one needs local write access to the
///   work tree, which is strictly more than a repository has. What this rules
///   out is a policy file the work tree does not own **and can be told apart by
///   its metadata** — not every file whose bytes live elsewhere.
pub(crate) fn classify_policy_source(rift_dir: &Path, path: &Path) -> PolicySource {
    match std::fs::symlink_metadata(rift_dir) {
        Err(_) => return PolicySource::Default,
        Ok(m) if !m.is_dir() => {
            return PolicySource::Refused(PolicyRefusal::RiftDirIsNotADirectory)
        }
        Ok(_) => {}
    }
    match std::fs::symlink_metadata(path) {
        Err(_) => PolicySource::Default,
        Ok(m) if m.is_file() => PolicySource::Declared,
        Ok(m) if m.is_symlink() => PolicySource::Refused(PolicyRefusal::ConfigIsASymlink),
        Ok(_) => PolicySource::Refused(PolicyRefusal::ConfigIsNotARegularFile),
    }
}

/// The configured checkpoint remote for `work_tree`, or `None` when the
/// repository has not named one.
///
/// **The single resolver.** Every consumer — the resume fetch ladder, the push
/// shipper, `rift sync` — reads the setting through this function, so "which
/// remote is the checkpoint remote" has exactly one answer per repository and
/// two callers cannot drift into disagreeing about it.
///
/// **There is deliberately no default.** An unconfigured repository resolves to
/// `None`, and every caller must treat that as "do not ship", never as
/// "`origin`". `origin` on a developer's machine is whatever project they
/// cloned; on the machine this was written on it is an unrelated third party's
/// repository, and a shipper that fell back to it would publish this
/// repository's capture ledger there.
///
/// The value is screened before it is returned because it becomes an argv word
/// in a `git` invocation: a leading `-` would be read as a flag, and
/// whitespace, NUL or a control byte cannot name a remote. A present-but-unsafe
/// value is an `Err` rather than a silent `None`, so a typo is reported instead
/// of masquerading as "not configured".
///
/// # Errors
/// [`RiftError::Config`] when `[checkpoint] remote` is present but is not a
/// usable remote token. Naming only the reason — never the value.
pub fn checkpoint_remote(work_tree: &Path) -> Result<Option<String>> {
    let rift_dir = work_tree.join(".rift");
    let path = rift_dir.join("config.toml");
    if !classify_policy_source(&rift_dir, &path).is_declared_policy_applied() {
        return Ok(None);
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(doc) = text.parse::<::toml::Value>() else {
        return Ok(None);
    };
    let Some(item) = doc.get("checkpoint").and_then(|c| c.get("remote")) else {
        return Ok(None);
    };
    let raw = item
        .as_str()
        .ok_or_else(|| RiftError::config("[checkpoint] remote must be a string"))?;
    if raw.is_empty() {
        return Err(RiftError::config("[checkpoint] remote is empty"));
    }
    if raw.starts_with('-') {
        return Err(RiftError::config(
            "[checkpoint] remote may not begin with '-' (it would be read as a git flag)",
        ));
    }
    if raw
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\0')
    {
        return Err(RiftError::config(
            "[checkpoint] remote may not contain whitespace or control characters",
        ));
    }
    Ok(Some(raw.to_owned()))
}
