//! Resolving the three lever tables against one family: what the pinned
//! capability table lets Rift write back, and what each lever may therefore
//! do. Its own module because this is the one place a cap is applied, and the
//! hook path reads its answer and nothing else.

use crate::{DocChannel, Formats, Mode, ServePolicy};

/// What the pinned capability table says this family lets Rift write back.
///
/// Three booleans rather than the table itself, so this crate stays a leaf: the
/// caller reads `rift_agent::respond::CAPABILITIES` — the one place a channel
/// is asserted from a committed pin — and hands the answer down. A family Rift
/// knows nothing about arrives here as [`FamilyCapability::default`], which is
/// "no channel at all", and every lever resolves to `off`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FamilyCapability {
    /// A pinned row declines a call with a reason **and** a recorded run shows
    /// that reason reaching the model. Documentation alone is not this flag.
    pub confirmed_reason_channel: bool,
    /// The family accepts a read's path amended to a Rift-owned sidecar.
    pub document_sidecar_rewrite: bool,
    /// The family accepts a content search given a head limit it lacked.
    pub grep_head_limit_rewrite: bool,
    /// A pinned artifact quotes this family's edit result telling the model
    /// that the file it just wrote is current in its context.
    ///
    /// Without it, the read an agent makes to check its own edit is the only
    /// sight of that edit it has, so nothing is ever declined on the strength
    /// of an edit line. The flag is a fact about the family's tooling, not
    /// about its hook API, and it caps no mode: it reaches the decision
    /// itself, which is the only place that knows a decline would have rested
    /// on an edit.
    pub pinned_edit_echo: bool,
}

/// Why a lever resolved below what the tables asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapReason {
    /// No recorded run shows this family's model receiving a decline's reason,
    /// so a decline here would be indistinguishable from a tool failure — which
    /// a model is right to retry.
    UnconfirmedReasonChannel,
    /// The family documents no amendment of a tool call, so there is nothing
    /// for this lever to do.
    NoAmendmentChannel,
}

impl CapReason {
    /// A short phrase for `rift recall status` and `rift doctor`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnconfirmedReasonChannel => "reason channel not confirmed by a recorded run",
            Self::NoAmendmentChannel => "no native document/search amendment channel",
        }
    }
}

/// One family's resolved levers: what each one may actually do, with the
/// numbers it reads. Every field is a decision already taken — the hook path
/// reads this and nothing else, so the rules live here rather than at each
/// call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The policy on disk could not be read, so every lever below is `off`.
    pub fail_closed: bool,

    /// Whether a decline may rest on the window's own edit at all, carried
    /// through from [`FamilyCapability::pinned_edit_echo`]. Not a mode and not
    /// capped by one: `enforce` on a family without it still declines the
    /// re-reads a *read* covers, and never the ones only an edit covers.
    pub pinned_edit_echo: bool,

    /// Declining an unchanged re-read.
    pub read_deny: Mode,
    /// What the tables asked for before the cap.
    pub read_deny_requested: Mode,
    /// Why `read_deny` sits below `read_deny_requested`, when it does.
    pub read_deny_cap: Option<CapReason>,
    /// The largest slice one decision may cover.
    pub max_slice_bytes: u64,
    /// The shortest slice one decision may act on, in lines. A shorter one is
    /// still decided and recorded, and never declined.
    pub min_slice_lines: u32,
    /// The largest file hashed inline on the gate path.
    pub max_inline_hash_bytes: u64,
    /// Tokens a recovery map may cost.
    pub recovery_budget: u32,
    /// How far a decline may rest on a subagent's lines inherited by name:
    /// the repository's `inherit_by_name`, never above `read_deny`.
    pub inherit_by_name: Mode,
    /// How far the first read of a large file may be answered with its bound
    /// outline: the repository's `first_contact`, never above `read_deny`.
    pub first_contact: Mode,
    /// The fewest lines a file has for `first_contact` to consider it.
    pub first_contact_min_lines: u32,
    /// The fewest bytes a file has for `first_contact` to consider it.
    pub first_contact_min_bytes: u64,
    /// How far the held part of a partly held read may be declined for the
    /// unheld range: the repository's `narrow`, never above `read_deny`.
    pub narrow: Mode,
    /// How far a read another live window of the repository holds may be
    /// declined with a cheaper route: the repository's `decline_family_held`,
    /// never above `read_deny`.
    pub decline_family_held: Mode,
    /// How far a decision may lean on lines a command's output was matched
    /// to a file as carrying: the repository's `recognise_by_output`, never
    /// above `read_deny`.
    pub recognise_by_output: Mode,

    /// Adding a head limit to a content search that carries none.
    pub grep_cap: Mode,
    /// Why `grep_cap` is `off` while `[search]` is not.
    pub grep_cap_off: Option<CapReason>,
    /// Answering a listing with the ranked top-N instead of running it.
    pub search_answer: Mode,
    /// Why `search_answer` sits below `[search] mode`, when it does.
    pub search_answer_cap: Option<CapReason>,
    /// The head limit added.
    pub grep_head_limit: u32,
    /// How many ranked paths an answer carries.
    pub glob_top_n: u32,
    /// Tokens an answer may cost.
    pub answer_budget_tokens: u32,

    /// Amending a document read's path to the sidecar.
    pub doc_rewrite: Mode,
    /// Why `doc_rewrite` is `off` while `[doc]` is not.
    pub doc_rewrite_off: Option<CapReason>,
    /// Declining a document read once with the sidecar named in the reason:
    /// the primary channel under `channel = "redirect"`, and under
    /// `channel = "rewrite"` the fallback a verified dropped amendment arms.
    pub doc_redirect: Mode,
    /// Why `doc_redirect` sits below `[doc] mode`, when it does.
    pub doc_redirect_cap: Option<CapReason>,
    /// Which channel this repository chose.
    pub doc_channel: DocChannel,
    /// Which formats may be converted.
    pub doc_formats: Formats,
    /// Rows kept per sheet.
    pub table_rows: u32,
    /// The largest document converted at all.
    pub max_input_bytes: u64,
    /// The characters-per-page floor below which a PDF is left alone.
    pub pdf_min_chars_per_page: u32,
    /// The gate-path conversion deadline.
    pub convert_budget_ms: u32,
    /// How long an unused sidecar is kept.
    pub ttl_days: u32,
}

/// Resolve the three tables against one family and what that family's pinned
/// rows allow.
///
/// Pure and allocation-free: the caller loads [`ServePolicy`] **from the file**
/// — `PreToolUse` opens no store — and calls this. Three rules do all the work.
///
/// 1. **A cap is a `min`.** A per-family override, the pinned capability and
///    the measured caveats can only hold a lever below what `[…] mode` asked
///    for. Nothing here lifts anything.
/// 2. **Anything that declines a call needs a confirmed reason channel**, and
///    ships at `observe` until its own window says otherwise (GOAL §2.10). A
///    repository that writes `enforce` gets it, capped by rule 1.
/// 3. **The two amendments are the exception the owner decided** (O14, O16):
///    they decline nothing, add nothing to the model's context and ask the
///    model for nothing, so on a family whose rows document the amendment they
///    act whenever their table is not `off`. `off` is still off.
#[must_use]
pub fn resolve(policy: &ServePolicy, family: &str, capability: FamilyCapability) -> Effective {
    let interception = &policy.interception;
    let search = &policy.search;
    let doc = &policy.doc;

    let override_mode = interception.agents.for_family(family);
    let requested = override_mode.unwrap_or(interception.mode);
    let (read_deny, read_deny_cap) = cap_decline(requested, capability);
    let (search_answer, search_answer_cap) = cap_decline(search.mode, capability);
    let (doc_redirect, doc_redirect_cap) = cap_decline(doc.mode, capability);

    // Rule 3. `channel = "redirect"` is a repository saying it would rather be
    // told than amended, so the amendment stands down and the decline above is
    // the primary channel.
    let (doc_rewrite, doc_rewrite_off) =
        if doc.mode == Mode::Off || doc.channel == DocChannel::Redirect {
            // A shut table and a repository that asked to be told instead are
            // both "no amendment wanted", which is not the same as a family
            // that cannot take one — hence no reason to report.
            (Mode::Off, None)
        } else if capability.document_sidecar_rewrite {
            // The table's own mode: an amendment acts in `enforce` and is
            // recorded without acting in `observe`, like every other lever.
            (doc.mode, None)
        } else {
            (Mode::Off, Some(CapReason::NoAmendmentChannel))
        };
    let (grep_cap, grep_cap_off) = if search.mode == Mode::Off {
        (Mode::Off, None)
    } else if capability.grep_head_limit_rewrite {
        (search.mode, None)
    } else {
        (Mode::Off, Some(CapReason::NoAmendmentChannel))
    };

    Effective {
        fail_closed: policy.fail_closed,
        pinned_edit_echo: capability.pinned_edit_echo,
        read_deny,
        read_deny_requested: requested,
        read_deny_cap,
        max_slice_bytes: interception.max_slice_bytes,
        min_slice_lines: interception.min_slice_lines,
        max_inline_hash_bytes: interception.max_inline_hash_bytes,
        recovery_budget: interception.recovery_budget,
        inherit_by_name: interception.inherit_by_name.min(read_deny),
        first_contact: interception.first_contact.min(read_deny),
        first_contact_min_lines: interception.first_contact_min_lines,
        first_contact_min_bytes: interception.first_contact_min_bytes,
        narrow: interception.narrow.min(read_deny),
        decline_family_held: interception.decline_family_held.min(read_deny),
        recognise_by_output: interception.recognise_by_output.min(read_deny),
        grep_cap,
        grep_cap_off,
        search_answer,
        search_answer_cap,
        grep_head_limit: search.grep_head_limit,
        glob_top_n: search.glob_top_n,
        answer_budget_tokens: search.answer_budget_tokens,
        doc_rewrite,
        doc_rewrite_off,
        doc_redirect,
        doc_redirect_cap,
        doc_channel: doc.channel,
        doc_formats: doc.formats,
        table_rows: doc.table_rows,
        max_input_bytes: doc.max_input_bytes,
        pdf_min_chars_per_page: doc.pdf_min_chars_per_page,
        convert_budget_ms: doc.convert_budget_ms,
        ttl_days: doc.ttl_days,
    }
}

/// Rules 1 and 2 for one lever that declines a call.
fn cap_decline(requested: Mode, capability: FamilyCapability) -> (Mode, Option<CapReason>) {
    if requested != Mode::Enforce {
        // Nothing to cap: `off` and `observe` decline nothing either way.
        return (requested, None);
    }
    if !capability.confirmed_reason_channel {
        return (Mode::Observe, Some(CapReason::UnconfirmedReasonChannel));
    }
    (Mode::Enforce, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_serve_policy, ServePolicySnapshot, ServePolicySource};

    /// Load a policy source through the real file path, so every test below
    /// exercises the loader the hook path uses rather than a parse helper.
    fn load_src(src: &str) -> (ServePolicySnapshot, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        std::fs::write(pol.join("serve.toml"), src).unwrap();
        (load_serve_policy(dir.path()), dir)
    }

    /// Claude Code as its pinned rows stand: a confirmed reason channel and
    /// both amendments.
    const FULL: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: true,
        document_sidecar_rewrite: true,
        grep_head_limit_rewrite: true,
        pinned_edit_echo: true,
    };

    /// Codex as its pinned rows stand: a documented decline no run has
    /// confirmed, and no amendment at all.
    const NO_CHANNEL: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: false,
        document_sidecar_rewrite: false,
        grep_head_limit_rewrite: false,
        pinned_edit_echo: false,
    };

    /// Antigravity as its pinned rows stand: confirmed, amends nothing, and
    /// its page says nothing about what its edit tools return.
    const CONFIRMED_ONLY: FamilyCapability = FamilyCapability {
        confirmed_reason_channel: true,
        document_sidecar_rewrite: false,
        grep_head_limit_rewrite: false,
        pinned_edit_echo: false,
    };

    fn every_lever(e: &Effective) -> [Mode; 5] {
        [
            e.read_deny,
            e.grep_cap,
            e.search_answer,
            e.doc_rewrite,
            e.doc_redirect,
        ]
    }

    /// The edit-echo fact travels from the pinned capability to the resolved
    /// levers untouched, and it caps nothing on the way: it is not a mode, and
    /// a family without it still enforces everything a *read* covers. What it
    /// governs is one arm of the decision, which is where the fact belongs.
    #[test]
    fn the_edit_echo_fact_is_carried_through_and_caps_no_lever() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nantigravity = \"enforce\"\n",
        );
        assert_eq!(loaded.source, ServePolicySource::Declared);

        let pinned = resolve(&loaded.policy, "claude-code", FULL);
        assert!(pinned.pinned_edit_echo);
        assert_eq!(pinned.read_deny, Mode::Enforce);

        let unpinned = resolve(&loaded.policy, "antigravity", CONFIRMED_ONLY);
        assert!(
            !unpinned.pinned_edit_echo,
            "nothing pinned says what this family's edit tools return"
        );
        assert_eq!(
            unpinned.read_deny,
            Mode::Enforce,
            "a named family still enforces the re-reads a read covers"
        );
        assert_eq!(unpinned.read_deny_cap, None);
    }

    /// **Nothing acts under a policy Rift could not read.** An unknown key, a
    /// value the tables do not accept, a family nobody pinned, a number
    /// outside its range and TOML that does not parse are one state with one
    /// answer: every active lever `off`, on every family, whatever that
    /// family's rows would have allowed.
    #[test]
    fn an_unreadable_policy_turns_every_active_lever_off() {
        for src in [
            "[interception]\nmode = \"enforce\"\nunknown_key = 1\n",
            "[interception]\nmode = \"aggressive\"\n",
            "[interception.agents]\ncursor = \"enforce\"\n",
            "[interception]\nmax_slice_bytes = 2097152\n", // exceeds the hash bound
            "[interception]\nmin_slice_lines = 0\n",       // a floor of zero is no floor
            "[interception]\nmin_slice_lines = -1\n",      // and a negative one is not a count
            "[interception]\nrecovery_budget = 0\n",
            "[search]\ngrep_head_limit = 0\n",
            "[search]\nglob_top_n = 0\n",
            "[search]\nanswer_budget_tokens = 0\n",
            "[doc]\nformats = \"office\"\n",
            "[doc]\nchannel = \"ignore\"\n",
            "[doc]\ntable_rows = 0\n",
            "[doc]\nmax_input_bytes = 0\n",
            "[doc]\npdf_min_chars_per_page = 0\n",
            "[doc]\nconvert_budget_ms = 0\n",
            "[doc]\nttl_days = 0\n",
            "[doc\nmode = \"enforce\"\n",
        ] {
            let (loaded, _d) = load_src(src);
            assert_eq!(
                loaded.source,
                ServePolicySource::Invalid,
                "expected {src:?} to be unreadable"
            );
            assert!(loaded.policy.fail_closed);
            for capability in [FULL, CONFIRMED_ONLY, NO_CHANNEL] {
                for family in ["claude-code", "codex", "antigravity"] {
                    let e = resolve(&loaded.policy, family, capability);
                    assert_eq!(
                        every_lever(&e),
                        [Mode::Off; 5],
                        "{src:?} left a lever live for {family}"
                    );
                    assert!(e.fail_closed);
                }
            }
        }
    }

    /// A policy file is a request, not a grant. Whatever `[interception]` and
    /// `[interception.agents]` ask for, a family reaches `enforce` only where
    /// a recorded run shows its model receiving the reason — otherwise the
    /// decision is still reached and recorded, and the call runs.
    #[test]
    fn a_per_family_override_cannot_exceed_the_pinned_capability() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\ncodex = \"enforce\"\nclaude-code = \"enforce\"\n",
        );
        let policy = &loaded.policy;

        let confirmed = resolve(policy, "claude-code", FULL);
        assert_eq!(confirmed.read_deny, Mode::Enforce);
        assert_eq!(confirmed.read_deny_cap, None);

        let unconfirmed = resolve(policy, "codex", NO_CHANNEL);
        assert_eq!(unconfirmed.read_deny_requested, Mode::Enforce);
        assert_eq!(
            unconfirmed.read_deny,
            Mode::Observe,
            "asked for, not granted"
        );
        assert_eq!(
            unconfirmed.read_deny_cap,
            Some(CapReason::UnconfirmedReasonChannel)
        );
        // The same family, the same file, with the channel confirmed: the
        // override is the only thing that was ever in question.
        assert_eq!(
            resolve(policy, "codex", CONFIRMED_ONLY).read_deny,
            Mode::Enforce
        );

        // A cap only ever holds a lever down. An override below the table's
        // mode is honoured as written, and lowering it needs no capability.
        let (lowered, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nclaude-code = \"off\"\n",
        );
        let e = resolve(&lowered.policy, "claude-code", FULL);
        assert_eq!(e.read_deny, Mode::Off);
        assert_eq!(e.read_deny_cap, None);
        let unnamed = resolve(&lowered.policy, "codex", FULL);
        assert_eq!(
            unnamed.read_deny_requested,
            Mode::Enforce,
            "an override names one family and speaks for no other: the table's \
             own mode is what the unnamed family is still asked for"
        );
        assert_eq!(
            unnamed.read_deny,
            Mode::Enforce,
            "retry telemetry never changes the requested mode"
        );
    }

    /// The shipped defaults actively use every pinned native capability.
    #[test]
    fn the_defaults_enforce_every_confirmed_lever_and_act_on_the_two_amendments() {
        let policy = ServePolicy::default();
        let e = resolve(&policy, "claude-code", FULL);
        assert_eq!(e.read_deny, Mode::Enforce);
        assert_eq!(e.search_answer, Mode::Enforce);
        assert_eq!(e.doc_redirect, Mode::Enforce);
        assert_eq!(e.grep_cap, Mode::Enforce);
        assert_eq!(e.doc_rewrite, Mode::Enforce);
        assert_eq!(e.doc_channel, DocChannel::Rewrite);
        assert_eq!(e.doc_formats, Formats::All);
        assert_eq!(e.grep_head_limit, 40);
        assert_eq!(e.glob_top_n, 8);
        assert_eq!(e.answer_budget_tokens, 160);
        assert_eq!(e.recovery_budget, 800);
        assert_eq!(e.table_rows, 200);
        assert_eq!(e.pdf_min_chars_per_page, 200);
        assert_eq!(e.convert_budget_ms, 250);
        assert_eq!(e.ttl_days, 30);
        assert!(!e.fail_closed);
    }

    /// ★ Retry-after-deny is telemetry, never a mode cap. All three supported
    /// families receive the same repository-wide enforcement request, while an
    /// existing explicit `observe` or `off` remains authoritative.
    #[test]
    fn retry_telemetry_never_demotes_a_supported_family() {
        let (wide, _d) = load_src("[interception]\nmode = \"enforce\"\n");
        for family in ["claude-code", "codex", "antigravity"] {
            let e = resolve(&wide.policy, family, CONFIRMED_ONLY);
            assert_eq!(e.read_deny_requested, Mode::Enforce, "{family} was asked");
            assert_eq!(e.read_deny, Mode::Enforce, "{family} was demoted");
            assert_eq!(e.read_deny_cap, None);

            let (observed, _d) = load_src(&format!(
                "[interception]\nmode = \"enforce\"\n\n[interception.agents]\n{family} = \"observe\"\n"
            ));
            let e = resolve(&observed.policy, family, CONFIRMED_ONLY);
            assert_eq!(e.read_deny, Mode::Observe, "{family} observe was ignored");
            assert_eq!(e.read_deny_cap, None);

            let (off, _d) = load_src(&format!(
                "[interception]\nmode = \"enforce\"\n\n[interception.agents]\n{family} = \"off\"\n"
            ));
            assert_eq!(
                resolve(&off.policy, family, CONFIRMED_ONLY).read_deny,
                Mode::Off
            );

            // Enforcement still requires the pinned reason channel.
            assert_eq!(
                resolve(&wide.policy, family, NO_CHANNEL).read_deny_cap,
                Some(CapReason::UnconfirmedReasonChannel)
            );
        }
    }

    /// An amendment reaches a family that documents one and no other; a family
    /// that amends nothing is told so rather than left looking merely idle.
    #[test]
    fn the_two_amendments_act_only_where_a_family_documents_them() {
        let policy = ServePolicy::default();
        let e = resolve(&policy, "codex", NO_CHANNEL);
        assert_eq!(e.grep_cap, Mode::Off);
        assert_eq!(e.grep_cap_off, Some(CapReason::NoAmendmentChannel));
        assert_eq!(e.doc_rewrite, Mode::Off);
        assert_eq!(e.doc_rewrite_off, Some(CapReason::NoAmendmentChannel));

        // `off` is off: an amendment is not exempt from the table being shut.
        let (shut, _d) = load_src("[search]\nmode = \"off\"\n\n[doc]\nmode = \"off\"\n");
        let e = resolve(&shut.policy, "claude-code", FULL);
        assert_eq!(
            every_lever(&e),
            [Mode::Enforce, Mode::Off, Mode::Off, Mode::Off, Mode::Off]
        );
        assert_eq!(e.grep_cap_off, None, "shut is not a missing channel");
        assert_eq!(e.doc_rewrite_off, None);
    }

    /// A repository that chooses `redirect` is choosing to be told rather than
    /// amended: the amendment stands down, and the decline it prefers is still
    /// held to the same evidence as every other decline.
    #[test]
    fn the_redirect_channel_stands_the_amendment_down() {
        let (redirect, _d) = load_src("[doc]\nmode = \"enforce\"\nchannel = \"redirect\"\n");
        let e = resolve(&redirect.policy, "claude-code", FULL);
        assert_eq!(e.doc_channel, DocChannel::Redirect);
        assert_eq!(e.doc_rewrite, Mode::Off);
        assert_eq!(e.doc_rewrite_off, None);
        assert_eq!(e.doc_redirect, Mode::Enforce);

        let e = resolve(&redirect.policy, "codex", NO_CHANNEL);
        assert_eq!(e.doc_redirect, Mode::Observe);
        assert_eq!(
            e.doc_redirect_cap,
            Some(CapReason::UnconfirmedReasonChannel)
        );

        // Under `rewrite` the decline is the fallback, not gone: a dropped
        // amendment has somewhere to fall back to, at the mode its table set.
        let (rewrite, _d) = load_src("[doc]\nmode = \"enforce\"\n");
        let e = resolve(&rewrite.policy, "claude-code", FULL);
        assert_eq!(e.doc_rewrite, Mode::Enforce);
        assert_eq!(e.doc_redirect, Mode::Enforce);
    }

    /// An amendment is a lever like any other: `observe` records it without
    /// acting. A repository that writes `observe` under `[doc]` or `[search]`
    /// must not find its reads amended anyway.
    #[test]
    fn observe_reaches_the_two_amendments_without_acting() {
        let (doc, _d) = load_src("[doc]\nmode = \"observe\"\n");
        let e = resolve(&doc.policy, "claude-code", FULL);
        assert_eq!(e.doc_rewrite, Mode::Observe);
        assert_eq!(e.doc_rewrite_off, None);
        let (search, _d) = load_src("[search]\nmode = \"observe\"\n");
        let e = resolve(&search.policy, "claude-code", FULL);
        assert_eq!(e.grep_cap, Mode::Observe);
        assert_eq!(e.grep_cap_off, None);
        // A family without the channel still cannot be amended in any mode.
        let e = resolve(&search.policy, "codex", NO_CHANNEL);
        assert_eq!(e.grep_cap, Mode::Off);
        assert_eq!(e.grep_cap_off, Some(CapReason::NoAmendmentChannel));
    }

    /// A family Rift has pinned nothing for is not a family it guesses about:
    /// its overrides are not its own, and it gets no lever.
    #[test]
    fn a_family_with_no_pinned_rows_gets_no_lever() {
        let (loaded, _d) = load_src(
            "[interception]\nmode = \"enforce\"\n\n[search]\nmode = \"enforce\"\n\n[doc]\nmode = \"enforce\"\n",
        );
        let e = resolve(&loaded.policy, "some-editor", FamilyCapability::default());
        assert_eq!(
            every_lever(&e),
            [
                Mode::Observe,
                Mode::Off,
                Mode::Observe,
                Mode::Off,
                Mode::Observe
            ],
            "it records; it never acts"
        );
        assert_eq!(
            loaded.policy.interception.agents.for_family("some-editor"),
            None
        );
    }

    /// A policy Rift refuses to read at all — a link where the file should be
    /// — is the unreadable case too, not a permissive one.
    #[cfg(unix)]
    #[test]
    fn a_refused_policy_turns_every_active_lever_off_as_well() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        let target = dir.path().join("outside.toml");
        std::fs::write(&target, "[interception]\nmode = \"enforce\"\n").unwrap();
        symlink(&target, pol.join("serve.toml")).unwrap();
        let loaded = load_serve_policy(dir.path());
        assert_eq!(loaded.source, ServePolicySource::Refused);
        let e = resolve(&loaded.policy, "claude-code", FULL);
        assert_eq!(every_lever(&e), [Mode::Off; 5]);
    }

    /// The first-contact lever ships at `observe` with its two floors, reads
    /// what is written, and is never above the read lever: a family whose
    /// channel no run confirmed, a repository at `off` and an unreadable file
    /// all hold it there or below, and a value it does not know shuts it.
    #[test]
    fn the_first_contact_lever_ships_observing_and_is_never_above_the_read_lever() {
        let shipped = resolve(&ServePolicy::default(), "claude-code", FULL);
        assert_eq!(
            (
                shipped.first_contact,
                shipped.first_contact_min_lines,
                shipped.first_contact_min_bytes
            ),
            (Mode::Observe, 400, 16_384)
        );
        let (written, _d) = load_src(
            "[interception]\nfirst_contact = \"enforce\"\nfirst_contact_min_lines = 600\n\
             first_contact_min_bytes = 20000\n",
        );
        let e = resolve(&written.policy, "claude-code", FULL);
        assert_eq!(
            (
                e.first_contact,
                e.first_contact_min_lines,
                e.first_contact_min_bytes
            ),
            (Mode::Enforce, 600, 20_000)
        );
        assert_eq!(
            resolve(&written.policy, "codex", NO_CHANNEL).first_contact,
            Mode::Observe
        );
        let (shut, _d) = load_src("[interception]\nmode = \"off\"\nfirst_contact = \"enforce\"\n");
        assert_eq!(
            resolve(&shut.policy, "claude-code", FULL).first_contact,
            Mode::Off
        );
        let (unknown, _d) = load_src("[interception]\nfirst_contact = \"always\"\n");
        assert_eq!(
            resolve(&unknown.policy, "claude-code", FULL).first_contact,
            Mode::Off
        );
    }

    /// The narrowing lever ships at `observe`, reads what is written, and is
    /// never above the read lever: a family whose channel no run confirmed, a
    /// repository at `off` and a value it does not know all hold it there or
    /// below. An enforcing family that leaves the key unwritten observes: the
    /// family's mode caps the lever and never lifts it.
    #[test]
    fn the_narrowing_lever_ships_observing_and_is_never_above_the_read_lever() {
        let shipped = resolve(&ServePolicy::default(), "claude-code", FULL);
        assert_eq!(shipped.read_deny, Mode::Enforce);
        assert_eq!(
            shipped.narrow,
            Mode::Observe,
            "an enforcing family, unwritten"
        );
        let (written, _d) = load_src("[interception]\nnarrow = \"enforce\"\n");
        assert_eq!(
            resolve(&written.policy, "claude-code", FULL).narrow,
            Mode::Enforce
        );
        assert_eq!(
            resolve(&written.policy, "codex", NO_CHANNEL).narrow,
            Mode::Observe,
            "a family whose reason channel no run confirmed"
        );
        let (off, _d) = load_src("[interception]\nnarrow = \"off\"\n");
        assert_eq!(resolve(&off.policy, "claude-code", FULL).narrow, Mode::Off);
        let (shut, _d) = load_src("[interception]\nmode = \"off\"\nnarrow = \"enforce\"\n");
        assert_eq!(resolve(&shut.policy, "claude-code", FULL).narrow, Mode::Off);
        let (unknown, _d) = load_src("[interception]\nnarrow = \"always\"\n");
        assert_eq!(
            resolve(&unknown.policy, "claude-code", FULL).narrow,
            Mode::Off
        );
    }

    /// The family-held lever ships at `observe`, reads what is written, and is
    /// never above the read lever: a family whose channel no run confirmed, a
    /// repository at `off` and a value it does not know all hold it there or
    /// below.
    #[test]
    fn the_family_held_lever_ships_observing_and_is_never_above_the_read_lever() {
        let shipped = resolve(&ServePolicy::default(), "claude-code", FULL);
        assert_eq!(shipped.decline_family_held, Mode::Observe);
        let (written, _d) = load_src("[interception]\ndecline_family_held = \"enforce\"\n");
        assert_eq!(
            resolve(&written.policy, "claude-code", FULL).decline_family_held,
            Mode::Enforce
        );
        assert_eq!(
            resolve(&written.policy, "codex", NO_CHANNEL).decline_family_held,
            Mode::Observe
        );
        let (off, _d) = load_src("[interception]\ndecline_family_held = \"off\"\n");
        assert_eq!(
            resolve(&off.policy, "claude-code", FULL).decline_family_held,
            Mode::Off
        );
        let (shut, _d) =
            load_src("[interception]\nmode = \"off\"\ndecline_family_held = \"enforce\"\n");
        assert_eq!(
            resolve(&shut.policy, "claude-code", FULL).decline_family_held,
            Mode::Off
        );
        let (unknown, _d) = load_src("[interception]\ndecline_family_held = \"always\"\n");
        assert_eq!(
            resolve(&unknown.policy, "claude-code", FULL).decline_family_held,
            Mode::Off
        );
    }

    /// The output-match lever ships at `observe`, reads what is written, and
    /// is never above the read lever: a family whose channel no run
    /// confirmed, a repository at `off` and a value it does not know all hold
    /// it there or below.
    #[test]
    fn the_output_match_lever_ships_observing_and_is_never_above_the_read_lever() {
        let shipped = resolve(&ServePolicy::default(), "claude-code", FULL);
        assert_eq!(shipped.recognise_by_output, Mode::Observe);
        let (written, _d) = load_src("[interception]\nrecognise_by_output = \"enforce\"\n");
        assert_eq!(
            resolve(&written.policy, "claude-code", FULL).recognise_by_output,
            Mode::Enforce
        );
        assert_eq!(
            resolve(&written.policy, "codex", NO_CHANNEL).recognise_by_output,
            Mode::Observe
        );
        let (off, _d) = load_src("[interception]\nrecognise_by_output = \"off\"\n");
        assert_eq!(
            resolve(&off.policy, "claude-code", FULL).recognise_by_output,
            Mode::Off
        );
        let (shut, _d) =
            load_src("[interception]\nmode = \"off\"\nrecognise_by_output = \"enforce\"\n");
        assert_eq!(
            resolve(&shut.policy, "claude-code", FULL).recognise_by_output,
            Mode::Off
        );
        let (unknown, _d) = load_src("[interception]\nrecognise_by_output = \"always\"\n");
        assert_eq!(
            resolve(&unknown.policy, "claude-code", FULL).recognise_by_output,
            Mode::Off
        );
    }

    /// The inherited-by-name lever ships at `observe`, reads `enforce` only
    /// where written, and is never above the read lever it qualifies: a
    /// family capped to `observe`, a repository at `off` and an unreadable
    /// file all hold it there or below.
    #[test]
    fn the_inherited_by_name_lever_is_never_above_the_read_lever() {
        let shipped = resolve(&ServePolicy::default(), "claude-code", FULL);
        assert_eq!(shipped.inherit_by_name, Mode::Observe);
        let (written, _d) = load_src("[interception]\ninherit_by_name = \"enforce\"\n");
        assert_eq!(
            resolve(&written.policy, "claude-code", FULL).inherit_by_name,
            Mode::Enforce
        );
        assert_eq!(
            resolve(&written.policy, "codex", NO_CHANNEL).inherit_by_name,
            Mode::Observe,
            "a family whose reason channel no run confirmed"
        );
        let (shut, _d) =
            load_src("[interception]\nmode = \"off\"\ninherit_by_name = \"enforce\"\n");
        assert_eq!(
            resolve(&shut.policy, "claude-code", FULL).inherit_by_name,
            Mode::Off
        );
        let (unknown, _d) = load_src("[interception]\ninherit_by_name = \"always\"\n");
        assert_eq!(unknown.source, ServePolicySource::Invalid);
        assert_eq!(
            resolve(&unknown.policy, "claude-code", FULL).inherit_by_name,
            Mode::Off
        );
    }
}
