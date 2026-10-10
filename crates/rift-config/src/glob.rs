//! A small glob matcher with a *guaranteed* polynomial bound (no backtracking).
//!
//! Supports the subset needed for `sensitive_paths`:
//! * `**` — any sequence of characters, including `/` (deep wildcard); an
//!   optional `/` immediately after `**` is consumed with it, so `**/x` also
//!   matches `x`;
//! * `*`  — any sequence of characters *except* `/`;
//! * `?`  — exactly one character except `/`;
//! * everything else is a literal.
//!
//! The pattern is tokenized once and matched with a dynamic-programming sweep:
//! one boolean row per token, one pass over the text per token. Worst-case work
//! is O(|pattern| · |text|) with O(|text|) memory and no recursion, so a hostile
//! committed policy cannot wedge the policy gates (ReDoS-style), as the
//! security model requires. Validation additionally caps pattern size
//! ([`MAX_PATTERN_LEN`], [`MAX_WILDCARDS`]) as defense in depth.
// Indexing justification: every index below is bounded by construction —
// `j` ranges over `0..=text.len()` against rows allocated with `text.len() + 1`
// slots, and `t[j]`/`t[j - 1]` are guarded by the same loop bounds.
#![allow(clippy::indexing_slicing)]

/// Maximum accepted glob pattern length in bytes (enforced at config
/// validation). Bounds the `|pattern|` factor of the matcher's O(p·t) cost.
pub const MAX_PATTERN_LEN: usize = 512;

/// Maximum accepted wildcard (`*`/`**`/`?`) count per pattern (enforced at
/// config validation). Redundant under the DP matcher — wildcards cost the same
/// as literals — but kept as defense in depth should the matcher ever change.
pub const MAX_WILDCARDS: usize = 64;

/// One pattern token after parsing `**`/`*`/`?`/literals.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tok {
    /// A literal byte.
    Lit(u8),
    /// `?` — exactly one non-`/` byte.
    AnyChar,
    /// `*` — any (possibly empty) run of non-`/` bytes.
    Star,
    /// `**` (optionally swallowing one trailing `/`) — any run, `/` included.
    DeepStar,
}

/// Parse the pattern into tokens, collapsing adjacent wildcard runs: a run of
/// `*`/`**` is equivalent to `**` if it contains one, else to `*`.
fn tokenize(p: &[u8]) -> Vec<Tok> {
    let mut toks: Vec<Tok> = Vec::with_capacity(p.len());
    let mut i = 0;
    while i < p.len() {
        let tok = match p[i] {
            b'*' if p.get(i + 1) == Some(&b'*') => {
                i += if p.get(i + 2) == Some(&b'/') { 3 } else { 2 };
                Tok::DeepStar
            }
            b'*' => {
                i += 1;
                Tok::Star
            }
            b'?' => {
                i += 1;
                Tok::AnyChar
            }
            c => {
                i += 1;
                Tok::Lit(c)
            }
        };
        match (toks.last_mut(), tok) {
            (Some(last @ (Tok::Star | Tok::DeepStar)), Tok::Star | Tok::DeepStar) => {
                if tok == Tok::DeepStar {
                    *last = Tok::DeepStar;
                }
            }
            _ => toks.push(tok),
        }
    }
    toks
}

/// Number of wildcard metacharacters (`*`, `?`) in `pattern`, counting `**` as
/// one. Used by validation to enforce [`MAX_WILDCARDS`].
#[must_use]
pub fn wildcard_count(pattern: &str) -> usize {
    tokenize(pattern.as_bytes())
        .iter()
        .filter(|t| !matches!(t, Tok::Lit(_)))
        .count()
}

/// Returns `true` if `text` matches the glob `pattern`.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let t = text.as_bytes();
    // `dp[j]` ⇔ the tokens consumed so far can match `text[..j]`.
    let mut dp = vec![false; t.len() + 1];
    let mut ndp = vec![false; t.len() + 1];
    dp[0] = true;
    for tok in tokenize(pattern.as_bytes()) {
        match tok {
            Tok::Lit(c) => {
                ndp[0] = false;
                for j in 0..t.len() {
                    #[cfg(test)]
                    work::add(1);
                    ndp[j + 1] = dp[j] && t[j] == c;
                }
            }
            Tok::AnyChar => {
                ndp[0] = false;
                for j in 0..t.len() {
                    #[cfg(test)]
                    work::add(1);
                    ndp[j + 1] = dp[j] && t[j] != b'/';
                }
            }
            Tok::Star => {
                ndp[0] = dp[0];
                for j in 1..=t.len() {
                    #[cfg(test)]
                    work::add(1);
                    ndp[j] = dp[j] || (ndp[j - 1] && t[j - 1] != b'/');
                }
            }
            Tok::DeepStar => {
                ndp[0] = dp[0];
                for j in 1..=t.len() {
                    #[cfg(test)]
                    work::add(1);
                    ndp[j] = dp[j] || ndp[j - 1];
                }
            }
        }
        std::mem::swap(&mut dp, &mut ndp);
        if !dp.iter().any(|&reachable| reachable) {
            return false; // no prefix of the text is reachable — fail fast
        }
    }
    dp[t.len()]
}

#[cfg(test)]
mod work {
    //! A count of the cells the matcher's sweep visits, compiled into the test
    //! build only.
    //!
    //! The property the matcher owes the policy gates is a work bound, not a
    //! speed: a wall-clock limit says nothing under a loaded machine and
    //! everything about the machine, while a count of inner-loop visits is the
    //! same everywhere. Thread-local, so tests running in parallel never see
    //! each other's steps.

    use std::cell::Cell;

    thread_local! {
        static STEPS: Cell<u64> = const { Cell::new(0) };
    }

    /// One call per inner-loop cell visit.
    pub(super) fn add(n: u64) {
        STEPS.with(|steps| steps.set(steps.get().saturating_add(n)));
    }

    /// The steps recorded on this thread since the last `take`, zeroing the
    /// count.
    pub(super) fn take() -> u64 {
        STEPS.with(|steps| steps.replace(0))
    }
}

#[cfg(test)]
mod tests {
    use super::{glob_match, tokenize, wildcard_count, work};

    #[test]
    fn literal_and_single_star() {
        assert!(glob_match("src/main.rs", "src/main.rs"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs")); // `*` does not cross `/`
        assert!(glob_match("src/*.rs", "src/main.rs"));
    }

    #[test]
    fn deep_star_crosses_slashes() {
        assert!(glob_match("**/.env*", ".env"));
        assert!(glob_match("**/.env*", "config/.env.local"));
        assert!(glob_match("**/*secret*", "app/db_secret.txt"));
        assert!(glob_match("**/id_rsa", "home/user/.ssh/id_rsa"));
        assert!(!glob_match("**/id_rsa", "home/user/id_rsa.pub"));
    }

    #[test]
    fn question_mark() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn empty_edges() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
        assert!(glob_match("*", ""));
        assert!(glob_match("**", "a/b/c"));
        assert!(glob_match("**/", "anything/at/all"));
        assert!(!glob_match("a", ""));
    }

    #[test]
    fn terminates_on_adversarial_pattern() {
        // many stars must not blow up
        assert!(glob_match("**/**/**/x", "a/b/c/x"));
        assert!(!glob_match("**/**/**/x", "a/b/c/y"));
    }

    /// The ReDoS-class input the naive recursive matcher choked on: k stars
    /// interleaved with literals against long, non-matching text is ~C(n,k)
    /// work under backtracking but O(p·t) under the DP sweep. The verdict is
    /// the count of cells the sweep visits, never a clock. The figures are
    /// exact because the sweep visits every one of a row's `text.len()` cells
    /// for each token it processes, and stops after the first row in which no
    /// prefix is reachable: the first two calls process every token, the third
    /// leaves after two (no `a` follows the leading `**` in that text).
    #[test]
    fn adversarial_star_literal_pattern_costs_one_sweep_per_token() {
        let pattern = "*a".repeat(15); // 15 stars, 15 literals
        let matching = "a".repeat(300);
        let non_matching = format!("{}b", "a".repeat(300));
        let deep = format!("**{}", "a?*".repeat(20));
        let deep_text = "x/".repeat(200);
        for (pattern, text, matches, cells) in [
            (&pattern, &matching, true, 30 * 300),
            (&pattern, &non_matching, false, 30 * 301),
            (&deep, &deep_text, false, 2 * 400),
        ] {
            work::take();
            assert_eq!(glob_match(pattern, text), matches);
            let visited = work::take();
            assert_eq!(visited, cells, "pattern={pattern:?}");
            assert!(visited <= sweep_bound(pattern, text));
        }
    }

    /// The count discriminates: the backtracking specification below, run
    /// through the same counter on an input small enough to finish, already
    /// takes far more steps than the sweep's bound for that input.
    #[test]
    fn a_backtracking_matcher_exceeds_the_sweep_bound() {
        let pattern = "*a".repeat(6);
        let text = format!("{}b", "a".repeat(30));
        work::take();
        assert!(!reference_matches(pattern.as_bytes(), text.as_bytes()));
        let backtracking = work::take();
        assert!(!glob_match(&pattern, &text));
        let sweep = work::take();
        let bound = sweep_bound(&pattern, &text);
        assert!(sweep <= bound, "sweep {sweep} over bound {bound}");
        assert!(
            backtracking > 100 * bound,
            "backtracking took {backtracking} steps against a bound of {bound}"
        );
    }

    /// `(tokens + 1) · (text.len() + 1)`: the polynomial ceiling the module
    /// doc promises, independent of how the sweep happens to be written.
    fn sweep_bound(pattern: &str, text: &str) -> u64 {
        let tokens = tokenize(pattern.as_bytes()).len() as u64;
        (tokens + 1) * (text.len() as u64 + 1)
    }

    #[test]
    fn wildcard_count_counts_collapsed_tokens() {
        assert_eq!(wildcard_count("a/b.rs"), 0);
        assert_eq!(wildcard_count("*/x/*"), 2);
        assert_eq!(wildcard_count("a?b*c"), 2);
        // adjacent stars collapse into one token (`**/` swallows the `/`,
        // making the following `*` adjacent; DeepStar·Star ≡ DeepStar)
        assert_eq!(wildcard_count("****"), 1);
        assert_eq!(wildcard_count("**/*.rs"), 1);
    }

    /// Reference implementation: the original (exponential) recursive matcher,
    /// kept test-only as an executable specification of the semantics. The
    /// proptest below checks the DP matcher agrees with it on small inputs
    /// (small enough that the reference cannot blow up).
    fn reference_matches(p: &[u8], t: &[u8]) -> bool {
        work::add(1);
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            b'*' if p.get(1) == Some(&b'*') => {
                let rest: &[u8] = if p.get(2) == Some(&b'/') {
                    &p[3..]
                } else {
                    &p[2..]
                };
                (0..=t.len()).any(|i| reference_matches(rest, &t[i..]))
            }
            b'*' => {
                let rest = &p[1..];
                let mut i = 0;
                loop {
                    if reference_matches(rest, &t[i..]) {
                        return true;
                    }
                    if i >= t.len() || t[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            b'?' => !t.is_empty() && t[0] != b'/' && reference_matches(&p[1..], &t[1..]),
            c => !t.is_empty() && t[0] == c && reference_matches(&p[1..], &t[1..]),
        }
    }

    proptest::proptest! {
        /// INVARIANT: the DP matcher implements exactly the recursive
        /// specification, for every pattern/text over the interesting alphabet.
        #[test]
        fn dp_matches_reference_spec(
            pattern in proptest::string::string_regex("[ab/*?]{0,10}").expect("valid regex"),
            text in proptest::string::string_regex("[ab/]{0,12}").expect("valid regex"),
        ) {
            proptest::prop_assert_eq!(
                glob_match(&pattern, &text),
                reference_matches(pattern.as_bytes(), text.as_bytes()),
                "pattern={:?} text={:?}", pattern, text
            );
        }
    }
}
