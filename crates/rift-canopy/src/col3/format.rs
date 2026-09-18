//! The number formats every panel shares: compact k/M for token-scale
//! figures and thousands grouping where rows must visibly sum.

/// Compact k/M formatting for token-scale numbers.
#[must_use]
pub fn fmt_km(v: f64) -> String {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{}", v.round() as u64)
    }
}

/// A count as the `f64` [`fmt_km`] takes. Counts here are token sums, which
/// stay well inside `f64`'s exact-integer range; a saturating cast is still
/// used so an absurd value degrades to a large number rather than to a wrong
/// one.
pub(super) fn f64_of(n: u64) -> f64 {
    n as f64
}

/// Thousands-grouped token count (`7862400` → `"7,862,400"`) for the
/// breakdown modal: its rows must *visibly* sum to the headline, so no lossy
/// k/M humanizing there. Tokens are counts, not money — money strings only
/// ever come pre-rendered from the money authority.
#[must_use]
pub fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.chars().count();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_count_groups_thousands() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1,000");
        assert_eq!(fmt_count(14_594_095), "14,594,095");
    }

    #[test]
    fn fmt_km_table() {
        assert_eq!(fmt_km(0.0), "0");
        assert_eq!(fmt_km(999.0), "999");
        assert_eq!(fmt_km(1_500.0), "1.5k");
        assert_eq!(fmt_km(10_000.0), "10k");
        assert_eq!(fmt_km(120_000.0), "120k");
        assert_eq!(fmt_km(1_250_000.0), "1.2M"); // {:.1} rounds half-to-even
        assert_eq!(fmt_km(3_400_000.0), "3.4M");
        assert_eq!(fmt_km(f64::NAN), "0");
        assert_eq!(fmt_km(-5.0), "0");
    }
}
