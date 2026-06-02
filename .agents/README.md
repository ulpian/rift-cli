# .agents/

Home for **agent-specific instruction overrides**. The canonical, shared
guidance lives in the repo root:

- [`CLAUDE.md`](../CLAUDE.md) — full guidance (read by Claude Code).
- [`AGENTS.md`](../AGENTS.md) — condensed cross-tool guidance (read by Codex,
  Cursor, Gemini, OpenCode, and other `AGENTS.md`-aware tools).

Put a file here only when a specific agent needs guidance that differs from the
shared instructions, named for that agent — e.g.:

```
.agents/
  codex.md       # Codex-specific notes / overrides
  cursor.md      # Cursor-specific notes / overrides
  gemini.md      # Gemini CLI-specific notes / overrides
```

Keep these minimal and additive; do not duplicate the shared instructions —
link back to `CLAUDE.md` / `AGENTS.md` instead. If guidance applies to every
agent, put it in `AGENTS.md`, not here.
