//! A small, linear glob matcher (no regex, no catastrophic backtracking).
//!
//! Supports the subset needed for `sensitive_paths`:
//! * `**` — any sequence of characters, including `/` (deep wildcard);
//! * `*`  — any sequence of characters *except* `/`;
//! * `?`  — exactly one character except `/`;
//! * everything else is a literal.
//!
//! Matching is bounded by the pattern/text length (no exponential blowup), so a
//! hostile committed policy cannot wedge the loader (ReDoS-style); see
//! `05-security` §5.5.
#![allow(clippy::indexing_slicing)]

/// Returns `true` if `text` matches the glob `pattern`.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    matches(pattern.as_bytes(), text.as_bytes())
}

fn matches(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        b'*' if p.get(1) == Some(&b'*') => {
            // `**` matches any run of characters (including `/`). An optional
            // trailing `/` after `**` is consumed so `**/x` also matches `x`.
            let rest: &[u8] = if p.get(2) == Some(&b'/') {
                &p[3..]
            } else {
                &p[2..]
            };
            (0..=t.len()).any(|i| matches(rest, &t[i..]))
        }
        b'*' => {
            // `*` matches any run of non-`/` characters.
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if matches(rest, &t[i..]) {
                    return true;
                }
                if i >= t.len() || t[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        b'?' => !t.is_empty() && t[0] != b'/' && matches(&p[1..], &t[1..]),
        c => !t.is_empty() && t[0] == c && matches(&p[1..], &t[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

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
    fn terminates_on_adversarial_pattern() {
        // many stars must not blow up
        assert!(glob_match("**/**/**/x", "a/b/c/x"));
        assert!(!glob_match("**/**/**/x", "a/b/c/y"));
    }
}
