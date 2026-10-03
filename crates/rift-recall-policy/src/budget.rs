//! The per-turn budget that bounds over-retrieval: how many tool calls and
//! how many returned tokens one turn may draw before the serve gate stops
//! answering. Its own module because it is state a caller carries through a
//! turn, while everything else in this crate is a decision taken once.

/// Default per-turn tool-call ceiling.
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 4;
/// Default per-turn returned-token ceiling.
pub const DEFAULT_MAX_TOKENS_PER_TURN: u32 = 2000;

/// A per-turn budget bounding over-retrieval.
#[derive(Clone, Copy, Debug)]
pub struct TurnBudget {
    max_tool_calls: u32,
    max_tokens: u32,
    calls_used: u32,
    tokens_used: u32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TOOL_CALLS, DEFAULT_MAX_TOKENS_PER_TURN)
    }
}

impl TurnBudget {
    /// A budget with explicit ceilings.
    #[must_use]
    pub fn new(max_tool_calls: u32, max_tokens: u32) -> Self {
        Self {
            max_tool_calls,
            max_tokens,
            calls_used: 0,
            tokens_used: 0,
        }
    }

    /// Try to account for one tool call. Returns `false` if the ceiling is hit.
    #[must_use]
    pub fn try_call(&mut self) -> bool {
        if self.calls_used >= self.max_tool_calls {
            return false;
        }
        self.calls_used = self.calls_used.saturating_add(1);
        true
    }

    /// Try to account for `tokens` returned. Returns `false` if it would exceed
    /// the ceiling (and accounts nothing). Uses saturating arithmetic so a
    /// hostile/huge `tokens` can never wrap the counter under the ceiling
    /// and slip through as if it fit.
    #[must_use]
    pub fn try_tokens(&mut self, tokens: u32) -> bool {
        if self.tokens_used.saturating_add(tokens) > self.max_tokens {
            return false;
        }
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        true
    }

    /// Remaining tokens this turn.
    #[must_use]
    pub fn tokens_remaining(&self) -> u32 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }

    /// Remaining tool calls this turn.
    #[must_use]
    pub fn calls_remaining(&self) -> u32 {
        self.max_tool_calls.saturating_sub(self.calls_used)
    }

    /// Anchor a new turn: zero the call/token draw-down while keeping
    /// the ceilings. A `rift_known_state` call resets the budget so
    /// every subsequent call in the turn draws from a fresh allowance.
    pub fn anchor(&mut self) {
        self.calls_used = 0;
        self.tokens_used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_caps_calls_and_tokens() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_call());
        assert!(!b.try_call(), "third call exceeds ceiling");
        assert!(b.try_tokens(60));
        assert!(!b.try_tokens(60), "would exceed token ceiling");
        assert!(b.try_tokens(40));
        assert_eq!(b.tokens_remaining(), 0);
    }

    #[test]
    fn token_counter_never_overflows_under_the_ceiling() {
        // A hostile huge `tokens` must be refused, not wrap the counter below the
        // ceiling and slip through (the saturating-arithmetic fix).
        let mut b = TurnBudget::new(4, 100);
        assert!(b.try_tokens(50));
        assert!(
            !b.try_tokens(u32::MAX),
            "u32::MAX must not wrap under ceiling"
        );
        assert_eq!(
            b.tokens_remaining(),
            50,
            "the refused charge accounts nothing"
        );
    }

    #[test]
    fn a_default_budget_holds_the_published_ceilings() {
        let mut b = TurnBudget::default();
        assert_eq!(b.calls_remaining(), DEFAULT_MAX_TOOL_CALLS);
        assert_eq!(b.tokens_remaining(), DEFAULT_MAX_TOKENS_PER_TURN);
        for _ in 0..DEFAULT_MAX_TOOL_CALLS {
            assert!(b.try_call());
        }
        assert!(!b.try_call(), "the default call ceiling stops the turn");
        assert!(!b.try_tokens(DEFAULT_MAX_TOKENS_PER_TURN + 1));
        assert!(b.try_tokens(DEFAULT_MAX_TOKENS_PER_TURN));
    }

    #[test]
    fn anchor_resets_draw_down_but_keeps_ceilings() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_tokens(90));
        assert_eq!(b.calls_remaining(), 1);
        assert_eq!(b.tokens_remaining(), 10);
        b.anchor(); // a known_state call anchors a fresh turn
        assert_eq!(
            b.calls_remaining(),
            2,
            "ceiling preserved, draw-down zeroed"
        );
        assert_eq!(b.tokens_remaining(), 100);
    }
}
