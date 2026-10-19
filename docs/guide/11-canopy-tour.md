# 11. A tour of Canopy

Canopy is the dashboard you get from bare `rift`: the policy files, capture
ledger and recall plane the command line drives, as a live three-column view of
one repository at a time. It *reads* the projection and policy files and writes
only the Project Settings values, the project registry and your own dashboard
preferences, never a captured fact. The machinery and the reasons behind every
interaction rule (modal parity, hints derived per stage, the close X) are in
[12-canopy-architecture.md](12-canopy-architecture.md); setup is
[01-quickstart.md](01-quickstart.md).

---

## 11.1 Opening it, and closing it again

```bash
rift                  # the dashboard
rift --ascii          # the plain renderer: no true colour, no 3D
rift --skip-splash    # or RIFT_SPLASH=never
```

Bare `rift` launches the dashboard **only when stdout is a terminal**; piped or
redirected it prints one plain-text frame, so a script that calls it by accident
gets text. `RIFT_HEADLESS` (any value) forces that path on a terminal: the data
plane runs for a bounded 1200 ms (`RIFT_HEADLESS_WAIT_MS`), one frame renders and
the command exits, even on a cold projection. `q` or
`Ctrl-C` quits with no "are you sure": every act is written when confirmed.

The splash plays once per *person*, not per repository (kept in
`~/.config/rift/canopy.toml`, [02-configuration.md](02-configuration.md)). `splash_mode` is `always`,
`first-run` or `never`; `reduced_motion = true` selects the static tier, and a
frame that misses its budget steps down a tier: a stuttering opening is worse than
a simpler one.

---

## 11.2 The frame

The **header** carries the mark, the version, a health dot and clickable actions
that are also keys: `? Help`, the identity button (your active profile),
`/ Palette`, `e Panel Editor`, `q Quit`. Below are three columns, projects /
detail / graphs, split 20/40/40 by default (§11.10). The **hint bar** lists the
keys live *where you are standing*, so it cannot advertise a key that does
nothing; a surface that answers no keys leaves it empty. **If the bar teaches it,
it works; if it is silent, nothing is listening.** The **command bar** at the foot
takes slash commands (§11.7).

---

## 11.3 Column 1: the projects rail

Every watched repository in your order, with a kind glyph (`▣` repository root,
`⌥` linked worktree under its family;
[09-worktrees-and-families.md](09-worktrees-and-families.md)), a name and a status
glyph. The toolbar `[+]` `[↑]` `[↓]` `[✕]` mirrors four keys.

| Glyph | Means |
|---|---|
| `●` | An agent session is active |
| `◐` | Initializing, or the projection is still ingesting |
| `○` | Registered, initialized, quiet |
| `·` | Registered, `rift init` not run anywhere |
| `◌` | Configured by the clone's `.rift/`, not set up on this machine; `s` opens setup |
| `×` | Not a git repository |
| `◒` | Setup unfinished (a step pending, or a projection without the hook install); `s` opens it |
| `◔` | A linked worktree of an initialized family that was never wired |
| `!` | An error; the row says which |

Every glyph has a one-cell plain substitute. `◔` is *not* "setup incomplete": an unfinished setup has a plan to resume, an
unadopted member has none and nothing it does is captured, so "resume setup" would
point at a door that does not exist. The act it wants is adoption. Removal hides a
project and erases nothing: the registry is append-only, so "removed" is a
superseding entry.

---

## 11.4 Column 2: the cards

Cards for the selected repository: **repository**, **models**, **activity**. In a
short column they collapse to a headline row rather than vanish: a missing card
and an empty card are different facts.

**Repository** shows name, kind, path, a status row and two buttons, the twins of
`r` and `s`. For an **unadopted member** the status row is itself the act: click it or press `A`. An error-severity
integration finding (hooks skipped, hook binary gone, lifecycle hooks silent, a
session left behind its own events) turns the status to `!` and reads `capture
failing — s / /settings`. It outranks running and idle, which would read as
healthy while capture is lost; a warning never turns the card.

**Models** is a treemap of model calls (`Σ N calls`). **Activity** is
thirty days of events, sessions and checkpoints as heat strips: density answers
*rhythm* at a glance; exact numbers are `rift dispatch`.

---

## 11.5 Column 3: the panels

Five panels: **token usage**, **system**, **recall**, **dispatch**, **recap**.

**Token usage** is a treemap and readout of the four token classes (`I` uncached
input, `R` cache reads, `O` output, `C` cache writes) and the cache-read fraction.
They stay four numbers because they are priced differently and summing them is a
lie with a plausible shape ([08-cost-and-savings.md](08-cost-and-savings.md)). The
clickable `Σ` total opens a read-only "by worktree" breakdown; sessions captured
before worktree provenance count as `unknown`, never relabelled.

**System** leads with **context pressure** in five neutral bands: **Clear** below
50%, **Moderate** below 70%, **High** 85%, **Very High** 95%, **Critical** above. The
names describe rather than judge, since a gauge that scolds gets ignored. A scope
line says *whose* context it is and never guesses. Events, tokens and serves over five
minutes are **event-anchored**: the window ends at the last event Rift saw, not
the wall clock, so an idle repository keeps its last real reading instead of
decaying to zero. `—` is unknown, never `0`.

**Recall** shows the savings headline (`saved 110.8M tokens · 1.4% · ~$412.30`),
serves, a period row, a read-vs-saved bar chart and a per-channel rate chart
([07-serving-and-injection.md](07-serving-and-injection.md)).

- *Headline.* `saved N tokens` is always there; the share
  (`avoided ÷ (avoided + read)`, as `rift savings` publishes it) only when
  answerable; the `~` dollars only when a seeded price applies. Narrow tiers shed
  whole segments from the right.
- *Period.* `[24h][7d][30d][all]`. Narrow windows are trailing, since a calendar
  day needs a timezone the dashboard should not hold, and share `rift savings
  --last`'s computation, so card and command cannot publish two arithmetics.
  **Every figure on the card is that one period's.**
- *Bars.* Read and avoided tokens on **one shared scale**, since independent bars
  draw a 1% saving like a 99% one. An empty window draws none: absence and a
  measured zero differ.
- *The headline is the door* to a read-only breakdown of the dollar estimate by
  model with its assumptions ([08-cost-and-savings.md](08-cost-and-savings.md)).

With Recall off it says `recall off — open Project Settings`. **Dispatch** is what
was captured (checkpoints, sessions, the agent / mixed / human split, the files
that carried the work); **Recap** is how agents are used (sessions by agent,
tokens, cost, recall's ledger). Their empty states differ on purpose.

---

## 11.6 The keys, in full

Keys route by context: an open overlay, then the mode, then the focused column.
Only the keys listed for your context act.

| Key | Where | Does |
|---|---|---|
| `j` `k` `↑` `↓`, `PgUp` `PgDn` | any column | Move / scroll |
| `Tab` / `Shift-Tab`, `←` / `→` | any | Cycle focus (projects, detail, graphs, command) / move between columns |
| `⏎` | projects | Open the selection into the detail column |
| `a` | any | Add a project  |
| `d` / `x` | projects | Remove the selected project |
| `Shift-↑` / `Shift-↓` | projects | Reorder (Shift-J/K arrive as plain capitals) |
| `s` / `r` / `e` / `p` | any | Project Settings / Recall Viewer / panel editor / profile menu |
| `w` / `S` / `1`–`4` | graphs | Worktree breakdown / savings breakdown / Recall period |
| `A` | detail, unadopted member | Adopt the member |
| `/` / `?` | any | Command palette / help |
| `q`, `Ctrl-C` | any | Quit |

`A` and `S` are capitals because the rail's `a` and `s` are taken, and a second
meaning on one key would make the rail's hint a lie. In the palette `↑`/`↓` move
the suggestion, `Tab` inserts it, `⏎` runs the line and `esc` discards it; Tab
never runs anything, since completion and execution are different intentions. An
open overlay owns the keyboard: results scroll with `j`/`k` and dismiss with
`esc`, and a confirm accepts **only** `⏎` and `esc`, so a keystroke already typed
cannot land on a confirmation you have not read.

---

## 11.7 The command palette

The grammar is **closed**: a fixed verb set, each parsing into a typed value, no
free-text command line and no shell; metacharacters are ordinary text, so a pasted
`;` or `$(…)` is a character in a project name. A parse failure keeps the buffer and offers the nearest verb. A verb's tier decides what
`⏎` does.

- **Dashboard**, typed effects: `/select` `/add` `/remove` `/relabel` `/settings`
  `/viewer` `/focus` `/profile` `/help` `/quit` (`/q`).
- **Read-only**, run in process by the code the CLI subcommand runs, the result in
  an overlay: `/status` `/memory` `/checkpoint` `/session` `/explain` `/doctor`
  `/verify` `/gc --dry-run` `/projects list` `/projects prune --dry-run`
  `/worktrees`.
- **Mutating**, the same delegation behind a confirm overlay naming the command
  and repository: `/init` (`/setup`) `/enable <agent>` `/disable <agent>`
  `/configure` `/rebuild` `/gc` `/projects prune` `/adopt`.

The repository is always the **selected project's path**, never the directory the
dashboard started in. **Nothing spawns a process**: delegation is a typed request
to an in-process handler, with no command string to inject into. **The hook entry
point has no verb**: nothing you type can express it.

---

## 11.8 Project Settings

`s`, the Repository card's button, or `/settings`: a centered modal with an `[x]`
and the hint row `j/k move · enter/space apply · pgup/pgdn scroll · esc/x close`
(`r` retries the last failed operation). Four sections, in order.

- **Recall**, four switches: the plane runs here; a bounded delta is injected each
  agent turn; opened documents are handed over as the Markdown sidecar; native
  search is index-answered and bounded. The last two are the `[doc]` and `[search]` levers
  ([02-configuration.md](02-configuration.md)): on writes `enforce`, off writes
  `off`, and a declared `observe` shows as on until you toggle it.
- **Capture**, three: tool calls, token usage, raw transcripts. Raw transcripts
  are on unless written off; switching them back on also writes the
  acknowledgement of what retaining them means.
- **Security / Policy**, read-only: policy validity, sensitive paths in effect,
  redactor packs ([04-redaction-and-secrets.md](04-redaction-and-secrets.md)).
- **Integrations**, every adapter in registry order with what was detected and what
  Rift manages ([10-agent-integrations.md](10-agent-integrations.md)); a finding
  prints `error:` or `warning:` with the remedy `rift doctor` prints.

**The repository's policy files are authoritative**: the modal changes only these
values. Edits are conflict-checked and atomic (symlinks and files changed underneath are
refused; comments survive), and Rift never overwrites a file it does not own. **A running operation freezes the modal**, so
the hint bar goes empty.

**An uninitialized repository** opens a resumable setup page: tick steps, apply
them, and `rift init` runs from inside the dashboard. Integration rows are
**pre-ticked for every supported agent detected on this machine**
([10-agent-integrations.md](10-agent-integrations.md) §10.5), so the default plan is
what `rift init` would do.

**A repository configured elsewhere.** "Initialized" means **this machine holds the
family's projection**, never that `.rift/` exists, since `.rift/config.toml` is
committed and travels with every clone. A fresh clone reads `◌`; Recall and Capture show the committed values as facts, not toggles, because setting
up a second machine must never rewrite the file the team shares, and its action
(`set up this machine: projection, git hooks, selected agents`) writes nothing
tracked. A non-git directory gets no wizard.

**Adding is initializing.** `[+]`, `a` and `/add` register the path, and the new
row's **first status read** decides: a repository never set up, or configured
elsewhere, is **set up there and then** with the setup page's default plan; one
already set up here, or a plain directory, gets a toast. The setup runs in a
**progress dialog**: the `running` frame opens before any work and each step
(`registry` through `recall`) lands marked `ok`, `..` in flight, `--` skipped or `!!` refused, the last
two with a reason. Failure names the step and leaves the plan staged for the setup page to resume. **Removing** takes the row out of view and touches
no configuration, hooks or projection; add it back and its first read finds it
set up.

---

## 11.9 The Recall Viewer

`r`, the card's button, `/viewer`, or `rift viewer`: a read-only surface replacing
the dashboard. Column 2 is the checkpoint catalogue; column 3 the selected checkpoint's
expandable timeline: summary, durable Recall state, the redacted transcript slice
and the assembly (files carried, with recorded hunks).
A checkpoint whose sessions sealed turns adds a **Turn seal** card: per session, the last turn-end seal's frontier, the lag of the live transcript past it, and whether a newer turn-end checkpoint holds a fresher transcript ([§3.7](03-capture.md#37-from-session-to-checkpoint-the-condense)).

| Key | Does |
|---|---|
| `Tab` / `Shift-Tab`, `←` / `→` | Cycle panes / move between them |
| `j` `k` `↑` `↓`, `PgUp` `PgDn`, `Home` `End` | Move, page, jump to an edge |
| `⏎` or `space` | Step into the next pane; on a timeline row, expand or collapse |
| `f` / `F` | Filter the catalogue / full-session view |
| `L` / `R` | Latest checkpoint / refresh |
| `/` | The palette, without leaving |
| `r` or `esc` / `q` | Back to the dashboard / quit |

It is **read-only by construction**, **never invents ordering** (an unknown order
reads `ordering unavailable`) and **labels its scope** (repository-wide totals are
not per-checkpoint facts). **Looking is not adopting**: launched standalone it
performs no ingest, and `--repo` on an unregistered repository stays session-only.

---

## 11.10 The panel editor

`e` or the header button: grips appear on every unit, dividers become grabbable
and the cursor moves over the layout.

| Key | Does |
|---|---|
| `h` `j` `k` `l`, arrows | Move the cursor (with a unit picked up, the unit) |
| `⏎`, `space`, `p` | Pick up / drop the unit under the cursor |
| `<` / `>` | Resize the divider right of the cursor's column |
| `[` / `]` | Move the whole column left / right |
| `r` / `esc` | Reset to default / cancel a pickup; again to leave |

Dragging a grip or divider does the same (the only drags Canopy tracks).
A drop is remove-then-insert, so a unit is never duplicated or lost (an emptied
column keeps a grabbable divider, so it is recoverable). The arrangement is saved per **(profile,
project)** as a superseding entry, so `r` is a new entry, never a deletion.

---

## 11.11 Profiles

`p` or the header's identity button. A profile is a named view with its own
project membership from the global catalogue, arrangements and theme. In the list
`j`/`k` move, `⏎` switches, `s` opens the profile's settings page, `n` creates, `d`
deletes (inline confirm; never offered for the active or the last profile) and
`esc` closes. A settings page has rows for name, colours, projects, duplicate,
startup pin and delete.

- **Creating** copies the *active* profile's project list and diverges;
  **duplicating** clones whole. Neither switches you: cloning is not switching.
- **Startup pin**: pinned, launch uses that profile; unpinned, the last selected.
- **Colours**: five slots (background, border, button, graph, text) with ten named
  presets and a reset each; the running dashboard is the preview.
- **Projects**: the ordered members editor (`d` remove, `Shift-↑/↓` reorder, `a` add
  from the catalogue projects the profile lacks).

Every profile act is an entry in an append-only log, and the live list is a fold
over it. Names are at most 24 characters and unique among live profiles. The palette speaks the same grammar: `/profile
new|rename|switch|delete|duplicate|startup [name]`.

---

## 11.12 Adding a project

`a`, `[+]` or `/add`. `a` opens a directory browser (browsing is the right
default, typing a path the fallback): `j`/`k` move, `⏎` descends, `⌫` goes up, `s`
selects the open folder, `i` types a path instead, `.` shows or hides dot-entries,
`esc` closes. A
listing failure arrives as a message, so an unreadable directory never ejects you. Registration is deduplicated by canonical path and then by
family, so a symlinked spelling or a linked worktree of a watched repository is
refused with an explanation, not added as a second row.

---

## 11.13 The mouse

Capture is on by default; `mouse = false` in `canopy.toml` or
`RIFT_CANOPY_NO_MOUSE` turns it off.
A left press acts on what is under it. The wheel acts on the region under the pointer without moving focus: over
the rail it moves the *selection* (its content is its rows), over the suggestion
panel the highlight, never inserting. Right and middle presses and plain moves do
nothing.

---

## 11.14 Small terminals, plain terminals

Four size tiers, chosen from the current size with no hysteresis.

| Tier | When | You get |
|---|---|---|
| Three columns | Width fits 16 + 32 + 28 cells | The full grid |
| Two columns | Fits the first two | Projects left; detail over graphs right |
| Single panel | Narrower | One panel at a time, with a tab strip |
| Notice | Under 24 columns or 6 rows | "terminal too small" |

Below a certain height the hint bar collapses: content wins over chrome.
`--ascii`, `NO_COLOR` or `RIFT_CANOPY_ASCII` force the plain floor (one-cell icons,
text treemaps): the same content, fewer characters.

---

## Related chapters

[01-quickstart.md](01-quickstart.md) · [02-configuration.md](02-configuration.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[10-agent-integrations.md](10-agent-integrations.md) ·
[12-canopy-architecture.md](12-canopy-architecture.md).
