# 11. A tour of Canopy

Canopy is the dashboard you get when you type `rift` with no arguments: the
same toolchain the command line drives — the same policy files, capture ledger
and recall plane — as a live, three-column view of one repository at a time.
The machinery underneath is
[12-canopy-architecture.md](12-canopy-architecture.md); if you have not run
`rift init` yet, start at [01-quickstart.md](01-quickstart.md).

> **Tip:** Everything Canopy shows is *read* from the projection and the policy
> files. The only things it writes are the seven Project Settings values, the
> project registry and your own dashboard preferences — never a captured fact.

---

## 11.1 Opening it, and closing it again

```bash
rift                  # the dashboard
rift --ascii          # the plain renderer: no true colour, no 3D
rift --skip-splash    # or set RIFT_SPLASH=never for one run
```

Bare `rift` launches the dashboard **only when stdout is a terminal**. Piped or
redirected — `rift | tee log`, `rift > out.txt` — it prints one plain-text frame
instead, so a script that calls `rift` by accident gets text rather than a
half-drawn interface. Set `RIFT_HEADLESS` to any value to force that path on a
real terminal (`RIFT_HEADLESS=1 rift --ascii`): one frame, plain lines, exit —
the shape for a script, a CI log or a bug report.

The headless frame is not a screenshot: it starts the same data plane the live
loop uses, lets it fill for a bounded moment (`RIFT_HEADLESS_WAIT_MS`, about a
second by default), renders once and stops. That bound exists so the command
always terminates — a dashboard that waited for "everything" would hang on a
cold projection.

**Quitting.** `q` from anywhere in the normal view, or `Ctrl-C` from anywhere at
all, including mid-edit inside a mode where every other key belongs to the mode.
There is deliberately no "are you sure": nothing holds unsaved state, because
every act is written the moment you confirm it.

### The splash

The first launch on a machine plays a short opening animation — a cracking
ground plane with the wordmark rising out of it — and any key or click skips it.
It plays **once per person, not once per repository**: having seen it is a fact
about you, so it lives in your user preferences (`~/.config/rift/canopy.toml`,
see [02-configuration.md](02-configuration.md)) rather than any project. Set
`splash_mode` to `always`, `first-run` or `never`, or run
`/configure splash never` from the palette.

The animation has a fidelity ladder: a full 3D tier when build and terminal both
support it, a 2D reduced tier, a static banner at the floor. It holds to a
per-frame budget, dropping resolution first and then a whole tier when it cannot
meet it — a stuttering opening is worse than a simpler one, and a dashboard must
never be slow to start because of decoration.

> **Tip:** If you have reduced-motion preferences, set `reduced_motion = true`
> in `canopy.toml` and the animation is replaced by the static tier outright.

---

## 11.2 The frame

At a comfortable terminal size you get this:

```text
 RIFT  Canopy v0.0.1  *              ? Help  p @ default  / Palette  e Panel Editor  q Quit
╭─ projects  1 ────╮╭─ repository ─────────────────────────╮╭─ token usage  Σ 7973.5M ─────────────#
│># rift-cli      o││rift-cli (repo)                       ││|R*********************************C*|#
│                  ││path: /Users/you/code/rift-cli        ││|##################################=#|#
│                  ││[ Recall Viewer ] [ Project Settings ]││input 4.4M read 7685.6M      read 100%#
│                  │╰──────────────────────────────────────╯│output 38.8M cache-write 244.7M       #
   … column 1 …    │      … column 2 …                     │      … column 3 …
 j/k navigate  ·  ⏎ open  ·  a add  ·  d remove  ·  ⇧j/k reorder  ·  / palette  ·  q quit
╭─ command ─────────────────────────────────────────────────────────────────────────╮
│/█                                                                                 │
╰───────────────────────────────────────────────────────────────────────────────────╯
```

Four pieces, top to bottom.

**The header** owns the top row: the product mark, the version, a health dot
for the data plane, and a right-aligned cluster of clickable actions — `? Help`,
the identity button carrying your active profile's name, `/ Palette`,
`e Panel Editor`, `q Quit`. One solution feeds both drawing and click test, so a
resize can never leave a button whose pixels and clickable cells disagree.

**Three columns** — projects, detail, graphs — in a 20/40/40 width split by
default. They are not hard-coded panes: the grid is a value you can rearrange
(§11.10), and the columns are what that value says today.

**The hint bar** sits one row above the command bar and lists the keys live
*where you are standing right now*, derived from the same tables the key reducer
dispatches on, so a hint can never advertise a key that does nothing. When a
surface answers no keys — while an operation runs — the bar goes empty rather
than keeping a stale list up. **If the bar teaches it, it works; if the bar is
silent, nothing is listening.**

**The command bar** at the foot is where slash commands are typed (§11.7).

> **Tip:** The one glyph without a matching key is the header's `? Help`:
> clicking it opens the help overlay, but there is no `?` key. Type `/help`.

---

## 11.3 Column 1 — the projects rail

Every repository Rift watches, in your chosen order, with a kind glyph (`▣` a
repository root, `⌥` a linked worktree indented beneath its family), a name and
a live status glyph. Worktrees are not separate projects but members of one
family with one shared identity
([09-worktrees-and-families.md](09-worktrees-and-families.md)). A toolbar at the
foot — `[+]` `[↑]` `[↓]` `[✕]` — mirrors four keys exactly: add a project, move
the selection up, move it down, remove it.

The status glyph on the right:

| Glyph | Plain | Means |
|---|---|---|
| `●` | `*` | An agent session is active right now |
| `◐` | `~` | Picking up: initializing, or the projection is still ingesting |
| `○` | `o` | Registered, initialized, quiet |
| `·` | `-` | Registered, but `rift init` has not been run anywhere |
| `◌` | `s` | Configured by the repository — its `.rift/` came with the clone — but not set up on this machine; `s` opens the setup |
| `×` | `x` | Not inside a git repository, so there is nothing to set up |
| `◒` | `~` | Setup is unfinished: a selected step is still pending, or the projection is here without the hook install; `s` opens the setup |
| `◔` | `a` | A linked worktree of an initialized family that was never wired |
| `!` | `!` | An error — the row says what it was: a read that failed, or a wired agent's capture failing |

The second column is what a terminal without full Unicode gets: every glyph has
a one-cell plain substitute, so the floor tier never renders a box of question
marks.

`◔` is deliberately *not* "setup incomplete". An unfinished setup has a plan on
disk you can resume; an unadopted worktree member has no plan at all, so nothing
it does is captured, and offering "resume setup" would point at a door that does
not exist. The act it wants is adoption, and the row says so.

Removal hides a project, it does not erase anything: the registry is
append-only, so "removed" is a new superseding entry over the old one and the
history stays intact — as with reordering and relabelling.

> **Tip:** The hint bar writes reorder as `⇧j/k`, but the dashboard receives
> **Shift with the arrows**: Shift-J and Shift-K arrive as ordinary capitals and
> do nothing, since reliable Shift reporting for letters needs a terminal
> protocol Canopy does not enable.

---

## 11.4 Column 2 — the cards

The detail column is a stack of bordered cards for the selected repository, in
a fixed default order — **repository**, **models**, **activity** —
sized by a shared stack solver from a weight, a minimum height and a priority. When the column is short, low-priority cards collapse to a single
headline row rather than vanishing — repository and activity hold out longest —
because a missing card and an empty card are different facts you cannot tell
apart if one is drawn as the other.

**The repository card** carries name and kind, path, a status row with its
coloured dot, and two buttons — the pointer twins of `r` (Recall Viewer) and `s`
(Project Settings). When the selected row is an **unadopted worktree member**
the status row is itself the act: drawn in the key-hint style, so the cells that
answer a click are the cells that look clickable, and clicking it or pressing
`A` adopts the member. `A` is bound *only* in that state, so it can never be
advertised where it would do nothing.
When an agent's integration has an error-severity finding (its hooks skipped,
its hook binary gone, its lifecycle hooks silent, or a session left behind its
own events), the status goes to `!` and the row reads `capture failing — s /
/settings`.
The finding outranks running and idle, because both would read as healthy
while capture is lost; it stays below a missing repository, a missing or
unreadable projection, an unadopted member and a corrupt session cache, whose
acts come first.
A warning never turns the card.

**The models card** is a treemap of model calls, keyed by letter, with a legend
underneath and a total (`Σ 700 calls`) in the title. Area is proportional to
call count, so the shape of the card *is* the shape of your model mix, and
models past the legend's room are summed into one honest `+N more` line rather
than dropped.

There is deliberately no capture card. Its three tiles — events, sessions,
checkpoints — counted exactly what the activity card strips over a month and
what the dispatch panel reports precisely, so it was a third rendering of
numbers already on screen, in the column that is always short. The counts are
where they always were ([03-capture.md](03-capture.md),
[05-ledger-and-verification.md](05-ledger-and-verification.md)); only the
duplicate card is gone.

**The activity card** shows thirty days of capture as three heat strips —
events, sessions, checkpoints — with a time scale beneath and each series'
maximum on the right. Density, not bars: a strip packs a month of daily counts
into a row you read at a glance for *rhythm*, which is what a month of activity
answers. Precise daily numbers are what `rift dispatch` is for.

---

## 11.5 Column 3 — the panels

Five panels, in default order: **token usage**, **system**, **recall**,
**dispatch**, **recap**. The column scrolls when they do not fit, and a
scrollbar appears on its right edge only when something is below the fold.

### Token usage

A proportional treemap of the four token classes — `I` uncached input, `R` cache
reads, `O` output, `C` cache writes — over a compact readout of the same four
numbers plus the cache-read fraction. Why four and not one "tokens" number is
[08-cost-and-savings.md](08-cost-and-savings.md): they are priced differently,
so summing them would be a lie with a plausible shape.

The `Σ` total in the title is clickable, and `w` — bound **only** while the
graphs column has focus — does the same: a read-only "by worktree" breakdown of
exactly that number, saying where each session was last observed and counting
sessions captured before worktree provenance existed as `unknown` rather than
relabelling them after the fact.

### System

The headline is **context pressure**: how full the model's context window is, as
a percentage and one of five neutral bands — **Clear** below 50%, **Moderate**
below 70%, **High** below 85%, **Very High** below 95%, **Critical** at 95% and
above — with a gauge fill sharing the same reading. The names are descriptive,
not judgemental: a full context window is a fact about a conversation, not a
mistake, and a gauge that scolds you gets ignored. The mapping is provably
monotonic, so a band can never move backwards as pressure rises.

A scope line says *whose* context this is — current sessions, the previous
session when nothing is live, or "session unavailable" when there is nothing to
read. It never guesses. Below it come events, tokens and serves over five
minutes, plus the projection store's size. Those rows are **event-anchored**,
not clock-anchored: the window ends at the last event Rift saw, not at the wall
clock, so an idle repository keeps the last real reading visible instead of
decaying to zero and implying activity stopped for a reason. `—` means the
number is genuinely unknown, and is never rendered as `0`.

### Recall

The savings headline (`saved 1.4% (110.8M)`), the serve count and active
sessions, then a **period row**, a **read-vs-saved bar chart**, and a
per-channel rate chart over the last thirty minutes — `hook` for injected turn
deltas, `skill` for calls an agent made itself
([07-serving-and-injection.md](07-serving-and-injection.md)).

The headline **is the savings share** — `avoided ÷ (avoided + read)`, the
figure `rift savings` publishes, to the same one decimal. It used to be a
second percentage with a different denominator, which looked exactly like this
one and sat two cells from a chip that answered the other question
([08-cost-and-savings.md](08-cost-and-savings.md)).

`[24h][7d][30d][all]` chooses the period the card's figures cover, by click or
with `1`–`4` while the graphs column has focus. The three narrow windows are
trailing, not calendar — a calendar "today" needs a timezone the dashboard has
no business holding — and each is handed to the *same* computation
`rift savings --last` uses, so the card and the command line cannot publish
two arithmetics. `all` adds no cut of its own, leaving the family's boundary
marker to decide exactly as "since the reset" always has, so an untouched
dashboard reads as it did before the buttons existed
([08-cost-and-savings.md](08-cost-and-savings.md)).

**Every figure on the card is that one period's**, resolved once and handed to
every read under it — the headline, the bars, the dollar estimate, the model
mix it is priced through, and the serve count beside the headline. A narrow
window composes with the family's boundary marker rather than replacing it, so
no button can show a row `rift savings` refuses to count; and a machine whose
clock cannot place a trailing window draws the empty window it is, saying
`no serves in this period — try [all]` rather than every row it holds under a
label that says `24h`.

The two bars are the window's read tokens and the tokens the plane avoided,
both from that one computation and **on one shared scale** — independently
normalised bars would draw a 1 % saving exactly like a 99 % one. `[tok]`/`[usd]`
at the row's right edge, or `$`, reads them as token counts or as dollars
through the same per-model rates and the same mandatory tilde the savings chip
uses. Asked for dollars with no seeded price for both sides, the card shows
tokens and says `recall · no seeded price` in its title rather than inventing a
rate; a window holding nothing on either side draws no bars at all, because
absence and a measured zero are different facts.

Two chips sit after the headline. The bracketed `[~$…]` is the dollar
estimate; `[of 90% target]` names what the share beside it is aiming at — a
goal the product states, not a figure it measured, from one constant the guide
and the card share. Either chip opens a read-only breakdown of how the dollar
estimate was composed, model by model, with its assumptions written out and
the share block under it: the window, the honesty figure, the `n`, the reads
the denominator could not weigh, and one row per family. `S`, while the graphs
column has focus, opens the same overlay through the same opener — the
breakdown used to be reachable by mouse and by nothing else, which is how a
keyboard user loses a feature quietly. What the figure means, why the `~` is
there, and how its denominator differs from the one `rift recap` prints are in
[08-cost-and-savings.md](08-cost-and-savings.md).

When Recall is off the panel says so and names the door:
`recall off — open Project Settings`.

### Dispatch and recap

The card halves of the two command-line reports. **Dispatch** is what this
repository captured: checkpoints and how their times were known (session-timed,
undated), sessions and how many are open, the agent / mixed / human attribution
split with its confidence, and the files that carried the work, sensitive ones
counted separately. **Recap** is how agents are being used: sessions by agent,
tokens, cost, and recall's ledger. Both carry their own empty states —
`(nothing captured yet)`, `(no agent sessions yet)`,
`(no capture projection here)` — which say different things.

---

## 11.6 The keys, in full

Canopy routes keys by context: an open overlay first, then the mode you are in,
then the focused column. Only the keys listed for your current context act; the
rest are inert — which is what the hint bar shows.

### Normal view

| Key | Where | Does |
|---|---|---|
| `j` `k` `↑` `↓` | any column | Move / scroll |
| `PgUp` `PgDn` | any column | Move / scroll by a page |
| `Tab` / `Shift-Tab` | any | Cycle focus: projects → detail → graphs → command |
| `←` / `→` | any | Move focus between columns only |
| `⏎` | projects | Open the selection into the detail column |
| `a` | any | Add a project (opens the browser) |
| `d` / `x` | projects | Remove the selected project |
| `Shift-↑` / `Shift-↓` | projects | Reorder the selected project |
| `s` | any | Project Settings for the selection |
| `r` | any | Recall Viewer |
| `e` | any | Panel editor |
| `p` | any | Profile menu |
| `w` | graphs | Break the token total down by worktree |
| `1`–`4` | graphs | The Recall card's period: 24h, 7 days, 30 days, all |
| `$` | graphs | Read the Recall bars in tokens or in dollars |
| `S` | graphs | Open the Recall card's savings breakdown (the chips' twin) |
| `A` | detail, unadopted member | Adopt the member |
| `/` | any | Command palette |
| `q`, `Ctrl-C` | any | Quit |

`e` used to launch a text editor; that behaviour is gone for good and the
mnemonic was reused for the *layout* editor. `A` is capital-A on purpose: the
rail's `a` already means "add a project", and a second meaning on one key would
make the rail's own hint a lie.

### While typing a command

| Key | Does |
|---|---|
| `↑` / `↓` | Move the suggestion highlight |
| `Tab` | Insert the highlighted suggestion — never run it |
| `⏎` | Run the line |
| `esc` | Close the palette, discarding the line |

Tab is always safe: completion and execution are different intentions and should
never share a key.

### While an overlay is open

An open overlay owns the keyboard: results scroll with `j`/`k` and dismiss with
`esc`, and a confirm accepts **only** `⏎` and `esc` — every other key is inert,
so a keystroke you had already typed cannot land on a confirmation you have not
read. A running dispatch swallows everything and hints nothing.

---

## 11.7 The command palette

`/` opens the palette, focuses the command bar and starts a suggestion panel
under your cursor, each row showing a verb, its usage and a one-line summary.

The grammar is **closed**: a fixed set of verbs, each parsing into a typed
value, with no free-text command line and no shell. Quoting is minimal — `"…"`
groups a token, every shell metacharacter is ordinary argument text — so a
pasted `;` or `$(…)` is a character in a project name, not a construct. The
buffer is length-capped, and a parse failure renders inline under the input with
the offending span underlined and the nearest verb offered as a hint, keeping
the buffer so you can fix the typo rather than retype the line.

Verbs fall into three tiers, and the tier decides what `⏎` does.

**Dashboard tier** — typed effects on the dashboard itself, no delegation:

`/select` · `/add` · `/remove` · `/relabel` · `/settings` · `/viewer` ·
`/focus` · `/profile` · `/help` · `/quit` (`/q`)

**Read-only tier** — delegated to the same code the CLI subcommand runs, in this
process, with the result rendered into a scrollable results overlay:

`/status` · `/memory` · `/checkpoint` · `/session` · `/explain` · `/doctor` ·
`/verify` · `/gc --dry-run` · `/projects list` · `/projects prune --dry-run` ·
`/worktrees`

**Mutating tier** — the same delegation, but a confirm overlay comes first,
naming the command it will run and the repository it will run against:

`/init` (`/setup`) · `/enable <agent>` · `/disable <agent>` · `/configure` ·
`/rebuild` · `/gc` · `/projects prune` · `/adopt`

Three properties of that door matter.

- **The repository is always the selected project's path**, never the directory
  the dashboard happened to be started in. A dashboard that can show you five
  repositories must never be ambiguous about which one a command lands on.
- **Nothing here spawns a process.** Delegation is a typed request handed to an
  in-process handler; there is no command string anywhere in the path, so there
  is nothing for an injected string to be injected *into*.
- **The hook entry point has no verb at all.** The agent- and git-facing wire
  entry is not merely hidden from the palette — no value in the verb set can
  express it, so no input you can type reaches it.

`/help` renders one usage line per verb into a results overlay.

---

## 11.8 Project Settings

`s`, the Repository card's button, or `/settings`. It opens as a centered modal
over the dashboard, titled `Project Settings · <repository>`, with an `[x]` at
the top right, an `About:` line at the foot (`Build and serve git-native
project memory; disabling keeps data.`) and, under it, the hint row
`j/k move · enter/space apply · pgup/pgdn scroll · esc/x close`.

Four sections, always in this order. **Recall** is four switches: whether the
recall plane runs here at all; whether a bounded delta is injected at the
start of each agent turn, the injection row showing the effective budget when
the repository declares a custom one; whether a document an agent opens is
handed to it as the family's Markdown sidecar; and whether native search
answers a name search from the index and bounds an uncapped content search.
The last two are the `[doc]` and `[search]` levers of the serving policy
([02-configuration.md](02-configuration.md)): on writes `enforce`, off writes
`off`, and a repository that declared `observe` shows as on with that detail
until the row is toggled. **Capture** is three — tool-call capture,
token-usage capture, raw transcripts — and enabling raw transcripts always
requires a separate acknowledgement of what retaining them means, with `esc`
backing out of it without closing the modal. **Security / Policy** is read-only
truth about the policy documents: declared or defaulted, valid or not, how many
sensitive paths are in effect, how many redactor packs loaded or failed
([04-redaction-and-secrets.md](04-redaction-and-secrets.md)).
**Integrations** lists every adapter in registry order with what was detected
and what Rift manages for it
([10-agent-integrations.md](10-agent-integrations.md)).
Under an adapter with an integration-health finding, each finding prints as
`error: <headline>` or `warning: <headline>` in the matching colour, with the
remedy sentence wrapped beneath it: the same sentence `rift status` and
`rift doctor` print.
The lines are not cursor stops and not click targets; the adapter row above
them stays the act, and moving the cursor onto it scrolls its remedy into view.
A healthy adapter prints nothing extra.

### The rules behind the surface

- **The repository's policy files are authoritative.** This modal is a front end
  for them, not a parallel store, and changes exactly the values above;
  everything else in `.rift/config.toml` is yours
  ([02-configuration.md](02-configuration.md)).
- **Edits are conflict-checked and atomic.** The shared editor refuses symlinked
  targets and a file that changed underneath it, writes to a sibling temporary
  file in the same directory and renames. Comments, formatting and unrelated
  keys survive.
- **Rift never overwrites a file it does not own.** An integration artifact with
  someone else's content is reported as a conflict, not "corrected".
- **A running operation freezes the modal.** While a change is applied the modal
  answers no keys and no clicks, and because it answers nothing it teaches
  nothing: the hint bar goes empty for exactly that moment. A bar advertising
  three keys over a surface that ignores all three is the defect the
  per-context rule exists to prevent.

### An uninitialized repository

The same modal opens as a resumable setup page titled `· setup required` (or
`· setup incomplete` once a plan is under way), with an action row at the foot
(`apply every selected local setup step`, and an `[ initialize project ]`
button). You tick the steps you want and apply them: `rift init` from inside
the dashboard, one selection at a time. The integration rows arrive
**pre-ticked for every supported agent detected on this machine** — by any of
the evidence classes in [10-agent-integrations.md](10-agent-integrations.md)
§10.5, an editor extension as much as a command on `PATH` — so the default
plan is "wire what is here", exactly what `rift init` would do.

### A repository configured elsewhere

"Initialized" means **this machine holds the family's projection**, never that
`.rift/` exists: `.rift/config.toml` is committed configuration and travels
with every clone. A fresh clone of a repository that shares its configuration
therefore reads `◌ not set up here`, and Project Settings opens titled `· not
set up on this machine`. The Recall and Capture rows show the committed values
as facts (`· [x] · committed by the repository`) rather than toggles, because
setting up a second machine must never rewrite the file the team shares; the
integration rows are pre-ticked as above; and the action row reads `set up
this machine: projection, git hooks, selected agents`. Applying it provisions
exactly those — plus the Recall skills and initial ingest when the committed
policy has Recall on — and writes nothing to the tracked files. A directory
that is not a git repository reads `× not a repository` and is offered no
wizard, since there is no family to provision.

### Adding a project

`[+]`, `a` and `/add` register the path, land the selection on the new row,
and let its **first status read** decide what happens next: a repository never
set up, or configured elsewhere, is **set up there and then** — `rift init`
with the plan the setup page would have opened with, the detected agents
included — and a repository already set up on this machine, or a plain
directory, gets the toast and nothing else. The decision waits for the read
rather than acting blind, because at the moment of the add nothing yet knows
which case it is. Adding a project *is* initializing it; the setup page stays
the place to change the choices afterwards, not a confirm step in the way of
the first one.

The setup runs in a **progress dialog**: the `running` frame opens before any
work, and each step lands as its own line — `registry`, `configuration`,
`projection`, `git hooks`, one line per agent, `policy`, `recall` — marked
`ok`, `..` (in flight), `--` (deliberately skipped, with the reason) or `!!`
(refused, with the reason). The marks are plain ASCII so the dialog reads the
same on every tier. When every step has landed the log stays up as a green
message until you close it; a failure leaves it up in red, naming the step
that failed and why, and the setup plan stays staged so the setup page can
resume it. Nothing in the dialog can be interrupted — like every `running`
frame it has no control to focus and swallows Esc — which is why it says what
it is doing while it does it.

Removing a project (`[✕]`, `d`/`x`, `/remove`) is the mirror of neither: it takes the row out
of the dashboard's view and touches nothing in the repository — not the
configuration, not the hooks, not the projection. Add it back and its first
read finds it already set up.

| Key | Does |
|---|---|
| `j` / `k`, `↑` / `↓` | Move the cursor |
| `PgUp` / `PgDn` | Scroll |
| `⏎` or `space` | Apply the row under the cursor |
| `r` | Retry the last failed operation |
| `esc` or `x` | Close |

Every row is clickable, and the `[x]` at the top right closes the modal. That
correspondence is a standing contract: **every act a surface teaches by key is
also reachable by click, and every clickable is reachable by key.**

---

## 11.9 The Recall Viewer

`r`, the Repository card's button, `/viewer`, or `rift viewer` from the shell —
a read-only inspection surface for one repository's captured history, in three
panes, replacing the dashboard. Column 1 stays the projects
rail; column 2 is the checkpoint catalogue, each entry carrying its timestamp
and how that time was known, its message, its checkpoint and commit ids and its
branch; column 3 is the selected checkpoint's timeline, in sections you can
expand — the checkpoint summary with its anchor and chain position, durable
Recall state, the redacted transcript slice, and the assembly, meaning the
files the checkpoint carried with their recorded hunks.

| Key | Does |
|---|---|
| `Tab` / `Shift-Tab` | Cycle panes: projects → checkpoints → timeline → command |
| `←` / `→` | Move between panes directly |
| `j` / `k`, `↑` / `↓` | Move within the pane |
| `PgUp` / `PgDn` | Page within the pane |
| `Home` / `End` | Jump to either edge — in the catalogue and the timeline |
| `⏎` or `space` | Step into the next pane; on a timeline row, expand or collapse it |
| `f` | Filter the catalogue (type, then `⏎` or `esc` to stop editing) |
| `F` | Toggle the full-session view |
| `L` | Jump to the latest checkpoint |
| `R` | Refresh the catalogue |
| `/` | The command palette, without leaving the viewer |
| `r` or `esc` | Back to the dashboard |
| `q`, `Ctrl-C` | Quit |

Four things make this surface trustworthy rather than merely convenient.

- **Read-only by construction.** Its state is navigation, filtering and
  expansion, all session-local; there is no write path for any field in it.
- **It never invents ordering.** When a checkpoint assembly's recorded order is
  unknown, the section says `ordering unavailable` rather than presenting an
  arbitrary sort as if it meant something.
- **It labels its own scope.** The Recall summary says outright that
  repository-wide totals are not per-checkpoint facts, so you cannot read a
  whole-project number as belonging to the checkpoint in front of you.
- **Looking is not adopting.** Launched standalone it performs no ingest, and
  `--repo` on an unregistered repository stays session-only — nothing joins your
  project list because you inspected it.

---

## 11.10 The panel editor

`e`, or the header's `e Panel Editor`. The grid stops being scenery and becomes
something you rearrange: grips appear on every unit, dividers between columns
become grabbable, and the cursor moves over the layout itself.

| Key | Does |
|---|---|
| `h` `j` `k` `l`, arrows | Move the cursor (or, with a unit picked up, move it) |
| `⏎`, `space`, `p` | Pick up the unit under the cursor / drop it |
| `<` / `>` | Resize the divider to the right of the cursor's column |
| `[` / `]` | Move the whole column left / right |
| `r` | Reset to the default arrangement |
| `esc` | Cancel a live pickup; press again to leave the editor |

With the mouse, press a grip and drag to move a unit, a divider to resize. This
is the one place Canopy tracks pointer drags: outside this mode moves and drags
are ignored outright, which is how an idle dashboard costs about nothing.

The atoms you move are the projects rail, the three cards and the five panels. A
drop is a remove-then-insert that conserves the set, so you can end up with a
card among the panels or an emptied column — which renders zero-wide and keeps a
grabbable divider, so it is recoverable — but never with a duplicated or lost
unit. The editor deliberately cannot **add** a unit that is not already on the
grid, or invent a panel: it moves what exists.

Your arrangement is saved per **(profile, project)** as a new superseding entry
in the dashboard's own append-only store — so `r`, the reset, is also a new
entry saying "back to the default", never a deletion of what you had.

---

## 11.11 Profiles

`p`, or the header's identity button. A profile is a named view of the
dashboard: its own project membership drawn from the global catalogue, its own
per-project arrangements, its own theme. The menu is a hub; from the browse
list:

| Key | Does |
|---|---|
| `j` / `k` | Move the cursor |
| `⏎` | Switch to the profile under the cursor — the whole view follows |
| `s` | Open that profile's settings page |
| `n` | New profile |
| `d` | Delete the profile under the cursor (opens an inline confirm) |
| `esc` | Close the menu |

`d` is guarded: the active profile and the last remaining one cannot be deleted,
and the key simply does not open the confirm for them. Deleting is never one
keystroke — the inline confirm is an explicit second act.

A profile's settings page carries six rows: **name**, **colours**, **projects**,
**duplicate**, **startup pin**, **delete profile**. `⏎` opens the row under the
cursor; `esc` steps back one stage rather than closing everything, all the way
out to the dashboard.

**Creating vs. duplicating.** Creating copies the *active* profile's project
list at that moment and then diverges, independent from the next edit onwards;
duplicating clones a profile whole — membership, per-project arrangements and
its set theme colours. Neither switches you to the result: cloning is not
switching.

**Startup pin** decides which profile the dashboard opens on: pinned, launch
uses that profile; unpinned, launch follows whichever you last selected.

**Colours** opens a two-stage picker over five theme slots — background, border,
button, graph, text — each with ten named presets plus a reset row. The running
dashboard is the preview, re-theming on every selection, so you choose against
the real thing rather than a swatch; `esc` steps back presets → slots → menu.

**Projects** opens the members editor: the profile's ordered project view, with
`j`/`k` to move, `d` to remove, `Shift-↑`/`Shift-↓` to reorder and `a` to open
the add stage, which lists exactly the catalogue projects this profile does not
already hold.

Every profile act — created, renamed, removed, selected, membership changed — is
a new entry in an append-only log, and the live list is a fold over that log:
the capture ledger's immutability rule
([05-ledger-and-verification.md](05-ledger-and-verification.md)) applied to a
much smaller thing, for the same reason.

Names are trimmed, non-empty, at most 24 characters and unique among live
profiles; a refused name renders inline and stays in the input.

> **Tip:** The palette speaks the same grammar — `/profile new|rename|switch|
> delete|duplicate|startup [name]` — so anything the menu does, a typed line
> does too. Two surfaces, one set of acts.

---

## 11.12 Adding a project

`a`, the rail's `[+]`, or `/add`. `a` opens a directory browser rather than a
text prompt: browsing is the right default, typing a path the fallback.

| Key | Does |
|---|---|
| `j` / `k` | Move the cursor |
| `⏎` | Descend into the directory under the cursor |
| `⌫` | Up one level (inert at the filesystem root) |
| `s` | Select the folder currently open |
| `i` | Switch to typing a path instead |
| `.` | Show or hide dot-entries |
| `esc` | Close |

Every one has a clickable twin on the surface, including a `[x]` that closes the
mode. `/add` registers the directory the dashboard was launched from —
explicitly, never automatically.

The row under the cursor wears a `>` in its first cell on every render tier:
the highlight is a glyph as well as a colour, so a plain terminal still shows
which row `⏎` will open. On Windows a **junction** — the reparse-point
directories a profile is full of: `My Documents`, `Application Data`, `Local
Settings` — says so beside its name, and one Windows refuses to list is an
inert, dimmed row rather than a directory the browser walks you into and then
cannot read.

The browser does no filesystem work inside the dashboard's own logic: a listing
arrives from outside, and a failure — a permissions error, a vanished directory
— arrives folded into it as a message rather than thrown, so a directory you
cannot read never ejects you from the flow. The message names the folder that
could not be listed (elided from the left, so its name survives), because you
are still in the folder you came from, and a bare `Access is denied` read as a
problem with the dashboard itself. A registration the store refuses names the
step it failed at the same way: `could not register <path> — canonicalize: …`.

Registration is deduplicated twice, by canonical path and then by family, so a
symlinked spelling and a linked worktree of a repository you already watch are
both refused with an explanation rather than added as a second row
([02-configuration.md](02-configuration.md),
[09-worktrees-and-families.md](09-worktrees-and-families.md)).

---

## 11.13 The mouse

Mouse capture is on by default. Turn it off in `canopy.toml` with
`mouse = false`, or for one run with `RIFT_CANOPY_NO_MOUSE` — worth knowing,
because capturing the mouse suppresses your terminal's own drag-to-select (most
terminals still copy on a Shift- or Option-drag).

- **Left press** acts on whatever is under it: a rail row selects, a header item
  runs its action, a card button opens its surface, a modal row activates, a
  `[x]` closes. Buttons flash to acknowledge the press; selection targets do
  not, because the row already has its own selection cue.
- **The wheel** acts on the region under the pointer without moving focus there:
  it scrolls the detail and graphs columns, a modal body, a results overlay, the
  settings modal and the directory browser; over the projects rail it moves the
  *selection*, since the rail's content is its rows; over the suggestion panel
  it moves the highlight and never inserts. In the Recall Viewer it does change
  panes, because there panes and focus are the same thing.
- **Right and middle presses do nothing**, and neither do plain pointer moves.
  The only drags Canopy tracks are the panel editor's.

Click regions are recomputed from the same geometry the renderer uses, never
recorded while drawing — two solvers would disagree after a resize and put your
clicks one cell off ([12-canopy-architecture.md](12-canopy-architecture.md)).

---

## 11.14 Small terminals, plain terminals

Canopy has four size tiers, chosen from the current size with no hysteresis, so
the same size always renders the same way:

| Tier | When | What you get |
|---|---|---|
| Three columns | Width fits all three minimums (16 + 32 + 28 cells) | The full grid |
| Two columns | Width fits the first two | Projects left; detail stacked over graphs right |
| Single panel | Narrower still | One panel at a time with a tab strip: `[Projects] Detail Graphs` |
| Notice | Under 24 columns or 6 rows | A centered "terminal too small" message |

Below a certain height the hint bar collapses to nothing rather than squeezing a
panel: content wins over teaching chrome, and it vanishes honestly instead of
half-drawn. The render tier degrades separately from the size tier —`--ascii`,
`NO_COLOR` or `RIFT_CANOPY_ASCII` force the plain floor, where every icon has a
one-cell printable substitute, the scrollbar's block thumb becomes `#`, and the
treemaps and charts draw in text. Nothing is hidden there: the same content,
drawn with fewer characters.

On Windows the tier is decided by the console itself: Canopy asks it to enable
virtual-terminal processing and, when it agrees (every Windows 10 console from
1703 on), draws in full colour — Windows Terminal and VS Code's terminal with
the Unicode glyphs, a bare console window with the ASCII icons its fonts can
show. `COLORTERM` and `TERM`, which no Windows host sets, are not consulted
there. A terminal that refuses mouse capture still gets the dashboard,
keyboard-only, with a notice saying so.

---

## Related chapters

[01-quickstart.md](01-quickstart.md) ·
[02-configuration.md](02-configuration.md) for `canopy.toml`, the registry and
the environment variables here · [06-recall.md](06-recall.md) and
[07-serving-and-injection.md](07-serving-and-injection.md) for what the Recall
panel counts · [08-cost-and-savings.md](08-cost-and-savings.md) ·
[09-worktrees-and-families.md](09-worktrees-and-families.md) ·
[10-agent-integrations.md](10-agent-integrations.md) ·
[12-canopy-architecture.md](12-canopy-architecture.md).
