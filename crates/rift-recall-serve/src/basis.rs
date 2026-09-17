//! Resolving the revision a caller says it holds, from a prefix of its hash.
//!
//! An agent that has a hash at all got it off a line of injected text, where
//! the whole 64-hex address costs about forty tokens and twelve or sixteen
//! characters cost four. So the pull verbs accept a **prefix**, and this module
//! is the one place a prefix becomes an address.
//!
//! Three rules, and the third is the one that matters:
//!
//! 1. **At least [`MIN_BASIS_HEX`](crate::basis::MIN_BASIS_HEX) characters, all hex.** Twelve is the
//!    display form and sixteen is the argument form; anything shorter is not
//!    an address, it is a coincidence waiting to happen.
//! 2. **A full 64-hex hash is its own resolution** and costs no query: a
//!    complete address matches at most one revision by construction, and a
//!    complete address nothing holds matches nothing downstream, which is the
//!    same answer this module would give.
//! 3. **Exactly one match resolves; zero or several are `Unknown`.** Never a
//!    guess, never the first row, never the newest. A caller whose prefix is
//!    ambiguous is told its revision is not known, and every verb answers an
//!    unknown basis the way it answers a moved file — "changed" — which costs
//!    a saving and can never serve the wrong file's lines as if they were
//!    this one's.

use rift_store::{CanonicalStore, Value};

/// The shortest prefix a caller may name a revision by. The manifest renders
/// twelve hex for a person to read and sixteen for a command to run; nothing
/// below this is accepted from either.
pub const MIN_BASIS_HEX: usize = 16;

/// The full length of a content address.
const FULL_HEX: usize = 64;

/// What a caller's `--known-hash` argument turned out to name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Basis {
    /// Exactly one revision in the family store carries this prefix.
    Resolved(String),
    /// Too short, not hex, or a prefix zero or several revisions carry. The
    /// caller's basis is not known — which is never the same sentence as "your
    /// file has not changed".
    Unknown,
}

impl Basis {
    /// The resolved hash, when there is one.
    #[must_use]
    pub fn hash(&self) -> Option<&str> {
        match self {
            Self::Resolved(h) => Some(h),
            Self::Unknown => None,
        }
    }
}

/// The lower-case hex form of `given`, or `None` when it is not a usable
/// prefix at all.
fn normalized(given: &str) -> Option<String> {
    let ok = given.len() >= MIN_BASIS_HEX
        && given.len() <= FULL_HEX
        && given.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| given.to_ascii_lowercase())
}

/// The exclusive upper bound of the range a prefix covers, for an ordered
/// comparison rather than a `LIKE` pattern: `LIKE` would make the caller's
/// text a pattern (`%` and `_` are wildcards in it), and a hash column is
/// indexed in byte order anyway. The last character is hex, so the successor
/// is always one ASCII step up and always sorts above every string the prefix
/// covers.
fn upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    let last = bytes.last_mut()?;
    *last = last.checked_add(1)?;
    String::from_utf8(bytes).ok()
}

/// Resolve one caller-supplied hash or prefix against the family store's own
/// content addresses.
///
/// Both relations that record one are consulted — the revisions the projection
/// holds and the content→blob bridge ingest writes — because a caller's basis
/// may be a revision this member never read but the family has, which is the
/// compounding the family store exists for.
///
/// # Errors
/// Propagates store read errors. A caller that would rather not fail at all
/// can treat an error as [`Basis::Unknown`]; that is what the dispatcher does.
pub async fn resolve_basis(store: &impl CanonicalStore, given: &str) -> rift_core::Result<Basis> {
    let Some(prefix) = normalized(given) else {
        return Ok(Basis::Unknown);
    };
    if prefix.len() == FULL_HEX {
        return Ok(Basis::Resolved(prefix));
    }
    let Some(upper) = upper_bound(&prefix) else {
        return Ok(Basis::Unknown);
    };
    // Two rows are all the question needs: one is a resolution and two is an
    // ambiguity, however many more there are.
    let rows = store
        .query(
            "SELECT content_sha256 FROM n_file_revision \
             WHERE content_sha256 >= ?1 AND content_sha256 < ?2 \
             UNION \
             SELECT content_sha256 FROM n_blob_oid_map \
             WHERE content_sha256 >= ?1 AND content_sha256 < ?2 \
             LIMIT 2",
            &[Value::text(&prefix), Value::text(&upper)],
        )
        .await?;
    match rows.as_slice() {
        [only] => Ok(only
            .get_text(0)
            .map_or(Basis::Unknown, |h| Basis::Resolved(h.to_ascii_lowercase()))),
        _ => Ok(Basis::Unknown),
    }
}

/// Resolve one caller-supplied hash or prefix **against one path's own
/// revisions**.
///
/// [`resolve_basis`] asks "does the family know this address"; this asks "is
/// this address a revision OF THIS FILE", and the difference is a security
/// boundary rather than a refinement. A content address is a caller-supplied
/// half of a two-half name: on its own it says which bytes, never which file.
/// A verb that gates policy on the path it was given and then fetches bytes by
/// the hash it was given has gated nothing — the caller chooses both halves
/// and can pair a harmless-looking basis with a file it may not read, or pair
/// an unrelated file's address with a sensitive path and be handed the whole
/// of it as "one changed place". That is the hash-only resolution
/// `Handle::File`'s contract refuses, and this function is how the pull verb
/// refuses it: both relations are asked with the path predicate, and an
/// address recorded for some other file resolves to nothing here.
///
/// A full 64-hex address is **not** its own answer here, unlike in
/// [`resolve_basis`]: completeness proves which bytes, and the question is
/// which file.
///
/// # Errors
/// Propagates store read errors. An error is [`Basis::Unknown`] to the
/// dispatcher, which is the conservative arm.
pub async fn resolve_basis_for_path(
    store: &impl CanonicalStore,
    given: &str,
    path: &str,
) -> rift_core::Result<Basis> {
    let Some(prefix) = normalized(given) else {
        return Ok(Basis::Unknown);
    };
    let Some(upper) = upper_bound(&prefix) else {
        return Ok(Basis::Unknown);
    };
    // The same ordered range as the family-wide resolver — never `LIKE`, whose
    // wildcards would make the caller's text a pattern — with `path` bound as
    // a value, and two rows still all the question needs.
    let rows = store
        .query(
            "SELECT content_sha256 FROM n_file_revision \
             WHERE path = ?3 AND content_sha256 >= ?1 AND content_sha256 < ?2 \
             UNION \
             SELECT content_sha256 FROM n_blob_oid_map \
             WHERE path = ?3 AND content_sha256 >= ?1 AND content_sha256 < ?2 \
             LIMIT 2",
            &[Value::text(&prefix), Value::text(&upper), Value::text(path)],
        )
        .await?;
    match rows.as_slice() {
        [only] => Ok(only
            .get_text(0)
            .map_or(Basis::Unknown, |h| Basis::Resolved(h.to_ascii_lowercase()))),
        _ => Ok(Basis::Unknown),
    }
}

/// Resolve every hash a request carries, dropping the ones that resolve to
/// nothing.
///
/// Dropping is the conservative direction and it is the whole behaviour change
/// prefixes bring to the three manifest verbs: a hash the store cannot resolve
/// never matched a current revision under exact comparison either, so a
/// dropped entry answers exactly as it did before — the file reads as changed.
pub async fn resolve_known_hashes(store: &impl CanonicalStore, given: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(given.len());
    for g in given {
        if let Ok(Basis::Resolved(h)) = resolve_basis(store, g).await {
            out.push(h);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use rift_store::{apply_migrations, recall_migrations, SqliteStore};

    use super::*;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(f)
    }

    /// A store whose blob bridge holds exactly `hashes` — the full-hash table,
    /// which is where two revisions sharing a prefix can be put on purpose.
    fn store_with(hashes: &[&str]) -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        for (i, h) in hashes.iter().enumerate() {
            block(s.insert_or_ignore(
                "n_blob_oid_map",
                &["content_sha256", "blob_oid", "path", "first_seen"],
                &[
                    Value::text(*h),
                    Value::text(format!("{i:040}")),
                    Value::text(format!("src/f{i}.rs")),
                    Value::Int(0),
                ],
            ))
            .unwrap();
        }
        s
    }

    /// Sixteen shared characters, then a difference: the collision a prefix
    /// argument can actually have, built deliberately rather than hoped for.
    const TWIN_A: &str = "abcdef0123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TWIN_B: &str = "abcdef0123456789bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const LONER: &str = "0f0f0f0f0f0f0f0f1111111111111111111111111111111111111111111111ff";

    /// ★ A prefix two revisions share resolves to neither of them. The verb
    /// that asked is then answering about a basis it does not know, which is
    /// the one answer that cannot serve one file's lines as another's.
    #[test]
    fn an_ambiguous_prefix_is_unknown_never_a_guess() {
        let store = store_with(&[TWIN_A, TWIN_B, LONER]);
        let shared = &TWIN_A[..MIN_BASIS_HEX];
        assert_eq!(shared, &TWIN_B[..MIN_BASIS_HEX], "the fixture's premise");

        assert_eq!(
            block(resolve_basis(&store, shared)).unwrap(),
            Basis::Unknown,
            "two revisions carry it, so it names neither"
        );
        // Not even the first one, the newest one, or the shortest path's one:
        // nothing about the ambiguous answer leaks which rows it saw.
        assert!(block(resolve_basis(&store, shared))
            .unwrap()
            .hash()
            .is_none());
        // One character more separates them, and then each resolves.
        assert_eq!(
            block(resolve_basis(&store, &TWIN_A[..17])).unwrap(),
            Basis::Resolved(TWIN_A.to_owned())
        );
        assert_eq!(
            block(resolve_basis(&store, &TWIN_B[..17])).unwrap(),
            Basis::Resolved(TWIN_B.to_owned())
        );
        // And the caller that holds the ambiguous prefix is left with no
        // basis at all rather than with a hash it did not name.
        assert!(block(resolve_known_hashes(&store, &[shared.to_owned()])).is_empty());
    }

    /// The path is half the name. An address this store really holds is still
    /// not a basis for a file it was never recorded against — and a *complete*
    /// address is no more authority than a prefix, because completeness says
    /// which bytes and the question is which file.
    #[test]
    fn an_address_recorded_for_another_file_does_not_resolve_for_this_one() {
        // `store_with` records the nth hash at `src/f<n>.rs`.
        let store = store_with(&[TWIN_A, TWIN_B, LONER]);
        assert_eq!(
            block(resolve_basis_for_path(
                &store,
                &LONER[..MIN_BASIS_HEX],
                "src/f2.rs"
            ))
            .unwrap(),
            Basis::Resolved(LONER.to_owned()),
            "its own path resolves it"
        );
        for given in [&LONER[..MIN_BASIS_HEX], LONER] {
            assert_eq!(
                block(resolve_basis_for_path(&store, given, "src/f0.rs")).unwrap(),
                Basis::Unknown,
                "another file's address is not this file's basis: {given:?}"
            );
        }
        // Within one path the twins are no longer twins: only one of them was
        // ever recorded there, so the shared prefix names it and nothing else.
        assert_eq!(
            block(resolve_basis_for_path(
                &store,
                &TWIN_A[..MIN_BASIS_HEX],
                "src/f0.rs"
            ))
            .unwrap(),
            Basis::Resolved(TWIN_A.to_owned())
        );
        // A path nothing was recorded against resolves nothing at all.
        assert_eq!(
            block(resolve_basis_for_path(&store, LONER, "src/nowhere.rs")).unwrap(),
            Basis::Unknown
        );
    }

    /// ★ The path-bound resolver's own "never a guess" arm. Two revisions **of
    /// one file** can share sixteen characters as easily as two revisions of
    /// different files can, and this is the resolver a caller reaches when it
    /// names a path — so the ambiguity has to be refused here too, not only in
    /// the family-wide one. Taking the first of several would hand a caller the
    /// changed lines of a revision it never held, under the numbering of one it
    /// did.
    #[test]
    fn an_ambiguous_prefix_of_one_files_own_revisions_is_unknown_never_a_guess() {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        // Both twins recorded at the SAME path, one in each relation, so the
        // union cannot collapse them and the path predicate cannot separate
        // them: the prefix names two revisions of this very file.
        block(s.insert_or_ignore(
            "n_blob_oid_map",
            &["content_sha256", "blob_oid", "path", "first_seen"],
            &[
                Value::text(TWIN_A),
                Value::text("oid_a"),
                Value::text("src/one.rs"),
                Value::Int(0),
            ],
        ))
        .unwrap();
        block(s.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text("rev_b"),
                Value::text("src/one.rs"),
                Value::text("oid_b"),
                Value::text(TWIN_B),
                Value::Int(10),
            ],
        ))
        .unwrap();

        let shared = &TWIN_A[..MIN_BASIS_HEX];
        assert_eq!(shared, &TWIN_B[..MIN_BASIS_HEX], "the fixture's premise");
        assert_eq!(
            block(resolve_basis_for_path(&s, shared, "src/one.rs")).unwrap(),
            Basis::Unknown,
            "two revisions of this file carry it, so it names neither"
        );
        // Not the first row, not the newest, not the one in either relation.
        assert!(block(resolve_basis_for_path(&s, shared, "src/one.rs"))
            .unwrap()
            .hash()
            .is_none());
        // One character more separates them, and then each resolves — so the
        // refusal above is the ambiguity and not the path predicate.
        assert_eq!(
            block(resolve_basis_for_path(&s, &TWIN_A[..17], "src/one.rs")).unwrap(),
            Basis::Resolved(TWIN_A.to_owned())
        );
        assert_eq!(
            block(resolve_basis_for_path(&s, &TWIN_B[..17], "src/one.rs")).unwrap(),
            Basis::Resolved(TWIN_B.to_owned())
        );
    }

    /// A prefix nothing carries is as unknown as one everything carries.
    #[test]
    fn a_prefix_nothing_carries_is_unknown() {
        let store = store_with(&[TWIN_A]);
        assert_eq!(
            block(resolve_basis(&store, &LONER[..20])).unwrap(),
            Basis::Unknown
        );
    }

    /// Sixteen is the floor and hex is the alphabet: everything else is
    /// refused before a query is made.
    #[test]
    fn nothing_shorter_than_the_floor_and_nothing_but_hex_is_a_basis() {
        let store = store_with(&[LONER]);
        for given in [
            "",
            "0f0f0f0f0f0f",     // the twelve-hex display form
            "0f0f0f0f0f0f0f0",  // fifteen
            "0f0f0f0f0f0f0f0g", // sixteen, but not hex
            "0f0f 0f0f0f0f0f0", // a space is not hex either
            "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0", // 65
        ] {
            assert_eq!(
                block(resolve_basis(&store, given)).unwrap(),
                Basis::Unknown,
                "{given:?}"
            );
        }
        // Sixteen exactly, unambiguous, resolves.
        assert_eq!(
            block(resolve_basis(&store, &LONER[..MIN_BASIS_HEX])).unwrap(),
            Basis::Resolved(LONER.to_owned())
        );
    }

    /// A whole address is its own answer, uppercase or not, and asks the store
    /// nothing.
    #[test]
    fn a_full_address_resolves_to_itself() {
        let empty = store_with(&[]);
        assert_eq!(
            block(resolve_basis(&empty, &LONER.to_ascii_uppercase())).unwrap(),
            Basis::Resolved(LONER.to_owned())
        );
    }

    /// The revisions relation answers too, not only the blob bridge: a caller
    /// may hold a revision the family projected before anything mapped its
    /// blob.
    #[test]
    fn a_revision_the_projection_holds_resolves() {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        block(s.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text("rev_1"),
                Value::text("src/a.rs"),
                Value::text("oid"),
                Value::text(LONER),
                Value::Int(10),
            ],
        ))
        .unwrap();
        assert_eq!(
            block(resolve_basis(&s, &LONER[..MIN_BASIS_HEX])).unwrap(),
            Basis::Resolved(LONER.to_owned())
        );
    }

    /// The same address in both relations is one revision, not an ambiguity —
    /// which is why the query is a union and not a concatenation.
    #[test]
    fn one_address_in_both_relations_is_still_one_revision() {
        let s = store_with(&[LONER]);
        block(s.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text("rev_1"),
                Value::text("src/a.rs"),
                Value::text("oid"),
                Value::text(LONER),
                Value::Int(10),
            ],
        ))
        .unwrap();
        assert_eq!(
            block(resolve_basis(&s, &LONER[..MIN_BASIS_HEX])).unwrap(),
            Basis::Resolved(LONER.to_owned())
        );
    }

    /// The resolver takes what the caller sends: a set of prefixes comes back
    /// as the addresses they name, in order, with the unknown ones gone.
    #[test]
    fn a_known_hash_set_comes_back_resolved_and_shortened() {
        let store = store_with(&[TWIN_A, TWIN_B, LONER]);
        let asked = vec![
            LONER[..MIN_BASIS_HEX].to_owned(),
            TWIN_A[..MIN_BASIS_HEX].to_owned(),
            TWIN_B.to_owned(),
        ];
        assert_eq!(
            block(resolve_known_hashes(&store, &asked)),
            vec![LONER.to_owned(), TWIN_B.to_owned()]
        );
    }
}
