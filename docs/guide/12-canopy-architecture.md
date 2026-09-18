# 12. How Canopy is built

[11-canopy-tour.md](11-canopy-tour.md) walks the dashboard from the outside;
this is the inside — the seams and the reasoning that put each one where it is.
You need it to *change* Rift: to add a panel, a key, a modal surface or a data
feed without breaking a property the rest of the toolchain leans on. Five do
most of the work, and everything below follows from one of them:

1. **The dashboard crate touches no storage.** It renders and reduces; it never
   reads a database, spawns a process, or knows what a projection is.
2. **Geometry is never stored.** Every clickable region is recomputed from the
   same solver that drew it, every frame.
3. **The loop is synchronous and idle-quiet.** A dashboard nobody touches does
   no work.
4. **Every act is reachable both ways.** What a surface teaches by key a pointer
   can reach; what a pointer can reach has a key.
5. **Nothing the dashboard writes is ever overwritten.** Its own state — project
   list, arrangements, profiles, colours — is an append-only log, the capture
   ledger's discipline.

## 12.1 Three crates, one direction

| Crate | Owns | Depends on |
|---|---|---|
| `rift-canopy` | The render seam, the pure reducer, the layout solvers, the hit-test, the theme, the splash | `rift-core`, `rift-registry` |
| `rift-canopy-data` | Every dashboard read and interface-state write: the bus and its worker, the change poller, the append-only store, the status/detail builders, the graph feeds, the viewer worker | the store, config, git, the recall crates, the gauge — and `rift-canopy` |
| `bins/rift` | Wiring: construct the store, spawn the bus, hand the channels and the seams to the loop | both of the above |

The important cell is the first row's last column. `rift-canopy` depends only on
the shared core types and the project-registry crate — the latter purely for its
config-directory resolver. No database, no async runtime, no filesystem walker,
no process spawner. The committed crate-graph snapshot under
`xtask/src/snapshots/` is what a reviewer reads to see whether an edge appeared.

**The edge points one way.** `rift-canopy-data` depends on `rift-canopy` for the
shared event and snapshot types and is never imported back, keeping the render
side storage-free while the data side grows. That is why the data plane became
its own crate: at roughly eighteen hundred lines inside the binary it could not
be tested without the command line, and it now runs against temporary-directory
projections in its own crate tests.

Everything the reducer needs from outside arrives through a **seam**, a small
trait the binary implements. There are six:

| Seam | What crosses it | Where the real one lives |
|---|---|---|
| `RenderBackend` | Frames out to a terminal | `crates/rift-canopy/src/render/` |
| `EventSource` | Keys, resize, and data updates in | the live terminal driver |
| `CommandDispatcher` | Delegated command-line verbs | `bins/rift` |
| `ProfileLoader` | Profiles, membership, arrangements, colours, the startup pin | the interface-state store |
| `ProjectSettingsManager` | Repository setup and integration changes | the shared native services |
| `Clock` | Time, for the splash only | the live monotonic clock |

Plus one closure-shaped seam for listing a directory and one for applying a
registry change. Tests inject scripted doubles for all of them, which is why most
of the dashboard is provable without a terminal.

---

## 12.2 The render seam

Every cell goes through `RenderBackend`, with exactly two implementations: **the
ASCII backend**, pure standard library, always compiled in and never optional,
and **the engine backend** behind the `xre` feature, driving the first-party
terminal engine with true colour, sub-cell shading and diffed presents.

Panels see neither: they draw into a Canopy-owned `Frame` — a grid of cells with
a glyph and a style — and the backend flushes it. The rule keeping this honest
is in the module's doc comment and enforced by review: **no module outside
`render/` names an engine type**, which makes an engine change a change to one
directory and guarantees the ASCII floor.

### What "can" means

Backend selection reads a Canopy-owned capability bitset, not the terminal: true
colour, full Unicode, sub-cell plotting, a luminance-ramp shader, mesh
rasterization, parallel rasterization, and the legacy-Windows-console path.
Detection splits in two — a thin probe reading the terminal and environment, a
pure mapping function turning probed facts into the bitset — so tests drive the
pure half and none reads a real terminal, and compiled-out features fold in with
a compile-time check, so a build without the 3D layer can never *claim* mesh
rasterization.

`--ascii`, `NO_COLOR` or `RIFT_CANOPY_ASCII` collapse the set to empty, and an
empty set is the ASCII floor. Selection degrades rather than fails: forced
ASCII, no true colour, *or the engine backend failing to initialize* all return
the floor. Startup degrades; it does not crash.

On Windows the probe is the console's own answer rather than the environment's.
The engine's probe reads `COLORTERM`, `TERM` and the locale, which Windows
Terminal, VS Code and conhost never set, so it reported sixteen colours and
ASCII on every Windows host and the dashboard always landed on the floor —
where the selection wash, being colour, vanished. Canopy instead asks the
console to enable virtual-terminal processing (the same call the engine's guard
makes) and maps that plus three host markers — `WT_SESSION`, `TERM_PROGRAM`,
`TERM` — through a pure function: colour follows VT; Unicode needs a host known
to render the glyphs; a console that refuses VT is the legacy floor. The
engine's presenter resolves every colour to a depth of its own at present time,
defaulting to that same environment probe, so backend selection overrides it
with Canopy's bit — without that the probe fix would select the engine and
still flush sixteen colours.

### The two feature names

The shipped `rift` binary turns on `tui`, enabling the crate's `xre` feature —
terminal layer, 2D interface layer, shading layer — and that is the standard
build. The 3D layer is a *second*, opt-in feature: `tui-hires` on the binary,
`xre-3d` on the crate, existing only for the splash's top fidelity tier and a
legacy high-resolution graph path. The feature table in
`crates/rift-canopy/Cargo.toml` is the authority.

The treemaps and activity strips produce engine-neutral sub-cell samples and ask
for a shader kind rather than drawing pixels: the standard backend resolves that
through the engine's cell shader, the default supplies a portable density
fallback, and the finer shader downgrades on a terminal without full Unicode. So
capability loss is *semantic*, not cosmetic — without colour you still get
density, outlines, separators, scales and values.

---

## 12.3 One solver, two readers

The reducer is a pure function: given a state and an event it returns a flow —
quit, or continue with a list of **effects** — and performs no input or output.
The loop performs the effects, so every test of dashboard behaviour tests a pure
function over synthetic events.

Rendering reads a **snapshot**, a view model built from the state with the
expensive parts shared by reference — the project rows and the arrangement each
carry a cache compared before replacement — so a frame that changes nothing
costs nothing to snapshot.

The hit-test — the function turning a click coordinate into a target — is a pure
function of that same snapshot, recomputing geometry from the *same* solvers the
renderer calls: the column split, the stack solver, the visible-row window, the
detail line kinds. It stores nothing.

> **Two solvers would eventually disagree.** If the renderer recorded rects as
> it drew and the hit-test read them back, a resize between draw and click puts
> your click one cell off. There is one solver, read twice, so drawn cells and
> clickable cells are the same cells by construction.

The hit-test is engine-free, so it compiles and tests without the terminal
feature, like the keyboard reducer it feeds.

The header's action cluster is the tidiest example: its five items — help,
identity, palette, panel editor, quit — are laid out by one function both the
renderer and the row-zero hit-test call. The identity, the only item whose width
varies, carries a **degradation ladder**: the profile name renders at a
sixteen-cell budget, shrinks stepwise with an ellipsis to a two-cell floor,
drops whole leaving the four static actions, and finally the cluster clips. A
width-sweep test pins the ladder monotonic, because bespoke geometry once made
the item vanish in a *band* of common widths while narrower and wider terminals
both showed it.

---

## 12.4 The synchronous loop

The dashboard loop is synchronous and takes no async runtime — ever; the crate
has no runtime dependency. It multiplexes by *draining merged sources*: the
terminal is pumped with a timeout, then any updates the data bus queued are
drained, and both flow into the one loop. The engine's event queue is blocking
by design, so a runtime would have added a thread and an abstraction to reach
the same semantics, and the load-bearing properties — a pure reducer, and a
dashboard that never redraws while quiescent — do not depend on the concurrency
model. Anything genuinely asynchronous lives behind the data plane's channels;
the decision re-opens only if the storage seam becomes async in a way worker
threads cannot contain.

### The loop body

```text
render if dirty  →  block for events (or the poll timeout)
                 →  advance any in-flight animation by the real elapsed time
                 →  reduce each event
                 →  perform the effects
                 →  repeat
```

Render-if-dirty is the whole redraw policy: the reducer sets a dirty flag when
something observable changed, and a frame is drawn only then; a timeout with no
events is a tick, and a tick at rest draws nothing. Two effects run *in the loop
body* rather than the generic applier — a delegated command dispatch and a
project settings action — because both do synchronous work that can block, so
their "running" frame must appear **before** the work starts.

### One injection point

Everything the live loop needs from its host arrives in a single struct, so
adding a seam means adding a field and wiring it once. That mattered once: an
"open the file in your editor" seam — the dashboard's one route to a process
spawn — was removed, its slot taken by what became the profile loader, and the
`e` key it freed went to the panel editor. **The strongest form of an invariant
is the one with no exceptions**: "the dashboard spawns nothing" is verified in
one read, while "spawns nothing except one property-tested seam" is not.

The no-spawn claim is not prose. `crates/rift-canopy/tests/no_spawn.rs` scans
every source file in the crate for the tokens that would mean a process could be
born — comments may name them, code may not — and fails if any appear. The only
door to the command line is the injected dispatcher, which receives *typed
requests*, never command strings.

### One startup path

The interactive launch and the one-frame headless render share their startup
seam by construction — both call the same profile-plane load and startup prime —
because when they did not, a live-data bug hid on the path the headless tests
never took. The headless frame renders a fixed hundred-by-forty grid through the
same draw function, so its output cannot drift, and runs the real bus for a
bounded window (about 1.2 seconds by default, adjustable) so the frame is *live*
rather than empty, and so the command always terminates.

---

## 12.5 The animator: one contract, six tweens

A dashboard that never redraws while idle and a dashboard with animation are in
obvious tension. The resolution is one contract every animation implements:

1. **State holds progress** — no timers, threads, extra event sources or clock
   reads inside the reducer.
2. **A pure advance-by-elapsed-time step moves it** and reports whether anything
   is still animating; the loop computes elapsed time outside the reducer.
3. **That report is the sole driver of the fast-poll window**: while something
   animates the loop polls at about thirty frames a second, and drops back to
   idle the moment nothing does.

The two cadences are 100 ms idle and 33 ms while settling, so fast frames are
paid only while something moves, and every animation is time-bounded. Six tweens
ride the contract:

| Tween | Settles in | What it is |
|---|---|---|
| Graph panel change | 350 ms | Column-three panels ease from the old shape to the new |
| Startup sweep | 450 ms | A one-shot reveal as the panels first fill; it never re-arms |
| Selection pulse | 300 ms | The rail's selection acknowledges a cursor move |
| Press flash | 300 ms | Any button acknowledges an activation |
| Suggestion fade | 180 ms | The completion panel fades in |
| Notice auto-dismiss | 2200 ms | A success message closes itself |

**The press flash is keyed by target, not by button.** One field pair — the
last-activated target and its progress — suffices, because a person activates
one control at a time; that is why re-pressing the *same* control flashes again
and why two cannot flash at once. A surface needing simultaneous flashes would
need a keyed map, which is where this design gets revisited rather than bent. **A keyboard activation arms the same pulse a click does**, the
animation half of §12.8's parity contract.

**Reduced motion and the ASCII floor degrade every animation to instant-show**,
and degraded is first class: the final frame, immediately, never a broken
half-state. Because progress advances by a passed-in duration, animations are
deterministic in tests by construction.

---

## 12.6 The data plane

The bus turns the reducer's data requests into scoped, **read-only** projection
queries and feeds results back as events, on its own thread with its own
current-thread runtime because the storage seam is async over a synchronous
engine. It has no write path. Failure degrades: a missing projection, a locked
database, an unreadable manifest become an error status on a card, not a panic.

### What "initialized" means to the bus

The bus classifies a project by whether **the family's projection exists under
the bus's state root** — the root it resolved once at spawn and passes into
every read — never by whether `.rift/` exists in the checkout. `.rift/` is
committed configuration that travels with a clone, so a bus that read it as
"set up" put a fresh clone straight into the initialized branch on every
machine and never offered the setup page. Two projection states carry the
distinction to the pure truth table (`Configured` → `NotProvisioned`,
`NotRepository` → `NotRepository`), and the Project Settings projection reads
the same file through the same root, so a card and its modal cannot disagree
about which page to open.

Both projections then ask one further question, of one further file: does the
family's **binary of record**, beside its manifest in the common dir, name a
binary that is still there? That file is what the git-hook install leaves
behind, and the projection cannot stand in for it — an agent hook opens the
store on first contact and creates one, so a family nothing has wired carries a
projection of its own. Without the record the card and the page agree on
`SetupIncomplete`, and the act that status asks for is the wizard, which
installs what is missing and re-runs safely over what is already there. The
hook decisions ask only for the projection, so nothing on a hook's path reads
this file.

The setup page's default plan is computed from the
installer's own diagnostic — the row is a pure projection of it, tested with
synthetic statuses — so the pre-tick and `rift init`'s auto-wire share one
detection predicate and cannot offer different agents. A view becomes a plan
through **one** reading (`ProjectSetupPlan::from_view`): the setup page's
`initialize` act and the add flow's `Init` act both take it from there, so the
two doors cannot provision different things from the same rows.

The add flow's act crosses the same injected write seam as the page's, through
a second method that carries a **progress sink**: the manager reports each
step as it starts and lands, and the loop re-presents the `running` frame on
every report while the synchronous work holds it — the running-frame posture
the dispatches already have, with a body. The page's own `apply` is that same
method with the sink dropped, so there is one implementation of a setup, not
two that drift.

### Change observation: the watermark

Each tracked project has a **watermark**, a pair: the **append cursor**, moving
whenever capture writes, and the **rebuild epoch**, moving only on a rebuild.
The epoch is explicit because inferring a rebuild from a cursor moving
*backwards* is unsound — a rebuild landing *more* rows looks like ordinary
ingest — so a rebuild is detected however the cursor moved, and re-bootstraps
every tracked project. The poller emits three change kinds: cursor moved at the
same epoch (new activity), epoch moved (rebuilt), watermark vanished
(unavailable).

### Filesystem notification is a doorbell, not a delivery

A native watcher runs where the platform supports one but carries **no data**: a
filesystem event under the cache root schedules an immediate watermark read, and
a fresh poll stays the only truth. A missed wake only delays, a duplicated wake
repeats a harmless read, a spurious wake converges on output the dirty gate
drops. Where watching fails — network mounts, exotic filesystems, a directory
not yet existing — the bus degrades to poll-only and the System panel reports
the mode.

| Cadence | Value | Why |
|---|---|---|
| Poll after a change | 250 ms | Something is happening |
| Poll when quiet | 2 s | Nothing is happening; one cheap read |
| Decay | 8 quiet polls | Back to idle without flapping |
| Dashboard re-stat, watched | every 600 idle ticks (~60 s) | Ages a session's status; drives no metric |
| Dashboard re-stat, poll-only | every 15 idle ticks (~1.5 s) | With no watcher the heartbeat *is* the wake |

A second, narrow watch covers the active project's family's session-cache
directories, the one change a cache-root watch cannot see. It is a *set*:
session caches stay per git directory and a family may run agents in any member,
so watching one vantage left a sibling worktree's session prompting nowhere.

### The channel cannot grow

Updates cross a **bounded** channel — sixty-four in flight — with a staging
buffer keeping the **latest per (project, feed)**, so a write storm can neither
stall the worker (sends never block) nor flood the loop, and superseded updates
are replaced while they wait. That works because **every feed emits full section
state, never a delta**: a dropped or duplicated delivery converges on the
fresh-read truth. Robustness is a property of the payload's shape, not of
heroics in the channel.

### Scope

Every query runs against the project's **own** databases — its repository
family's, resolved through the family identity — so two checkouts of one
repository read one store and two repositories never read each other's.
Cross-project spend cannot leak into a panel, tested over multi-repository
fixtures.

### The gauge has one owner

The context-pressure reading is not computed in the dashboard: `rift-nic-gauge`
owns the formula, the five bands and the model-to-window reference table, and
the data plane consumes it through a trait, so drift ends by construction. Two
things about that crate are load-bearing:

- **The bands are neutral and monotonic** — Clear, Moderate, High, Very High,
  Critical, at fifty, seventy, eighty-five and ninety-five percent — with a
  property test pinning the ordering to fullness. Editorializing names, or an
  evenly-spaced ladder calling healthy sessions alarming, are what that avoids.
- **An unknown model is explicit in the type.** No percentage can be got from an
  unknown-model reading, so absent reference data cannot masquerade as headroom:
  the "silent infinite headroom" of assuming a default window size is
  unrepresentable rather than merely avoided.

---

## 12.7 The layout is data

A panel editor needs to reorder units, move them across columns, reorder whole
columns and resize widths, so the arrangement — once three coupled constants —
had to become a value. A **layout model** is an ordered list of columns; a
column is a width weight and an ordered list of **units**; a unit is the
projects rail, a card or a panel. The left-to-right column order *is* the column
order, and units carry ids rather than indices, so a saved arrangement survives
reordering.

The default reproduces the previous grid exactly — rail at weight twenty, the
three cards at forty, the five panels at forty. The old three-way split
generalized into an n-way split with a pinned remainder rule, and a property
test asserts the two agree at *every* width. Column minimums are derived, not
declared: a column's minimum is the widest among its units, reproducing the
sixteen / thirty-two / twenty-eight cell floors the responsive tiers key on.

One solver, `solve_model`, tiles the columns; another, `solve_column`, maps a
heterogeneous unit list through the unchanged vertical stack solver. Renderer
and hit-test iterate the *same* output — the only way §12.3's rule survives a
movable layout — and exact tiling holds because no rect is stored: every frame
re-tiles.

### What the editor can and cannot do

The editor is a **mode** with its own reducer branch — as the command line, the
destination browser and the overlays each fence their own grammar — and keyboard
is the primary path, which makes it deterministic and terminal-independent. The mouse drag is a *scoped exception* to the loop's ignore-drags rule,
scoped exactly: drag and release are handled **only** when the mode is the panel
editor **and** a pickup or divider drag is already live. Everywhere else the
fall-through is unchanged and does not dirty, and plain pointer motion is always
ignored — which is why an idle dashboard costs about nothing in any mode.

A drop is a remove-then-insert conserving the unit set, so you can end up with a
card among the panels or an emptied column, never a duplicated or lost unit;
grips, dividers and drop targets come from the same solver the renderer used,
since stored drag geometry would desync on a resize mid-drag. Editing happens in
the full three-column tier only: the narrow tiers restructure by *role*, where a
reordered model has no meaning, so a small terminal shows the canonical layout.

### Saving an arrangement

An arrangement is persisted as an **append-only event** keyed by profile and
project, a reset recorded as a superseding event carrying no arrangement, never
as a deletion (§12.10). The encoding is a small versioned string — a version
tag, then per column a weight and a comma-separated unit list — parsed by a
hand-written codec, because the pure render crate carries no serialization
framework. Decoding returns nothing on a malformed or forward-version value, so
a store written by a newer binary degrades to the canonical layout rather than
crashing.

One deliberate exception to "unknown tokens fail closed": four retired card
names — three that moved into Project Settings, and the capture card that was
removed outright — are skipped by name in an old saved arrangement, because
known-retired names are a fixed, enumerable list. Every *other* unknown
token still fails the whole decode, since treating all unknowns as skippable
would silently accept corruption.

---

## 12.8 Modal geometry and the parity contract

### The frame

A modal is a centred box at eighty percent of the terminal each way, clamped
*up* to a readable forty by twelve cells and *down* to the region between the
one-row header and the bottom chrome, so header, hint and command bars stay
visible. On the narrow tiers it degrades to ninety-five percent so its content
stays usable. One function computes that rect and feeds draw and hit-test.

The dashboard stays drawn behind it, dimmed by a themed scrim — glyphs kept, so
the body reads as *behind* rather than gone. The dim is colour-only, so the
monochrome and ASCII tiers drop it and the border alone separates the modal, and
the hit-test returns nothing outside the rect, so a click in the dimmed body is
inert. Centring is the point: top-anchored, near-full-width overlays read as
*the whole screen went modal*.

Chrome is never modalized — header, hint and command bars, the suggestion panel,
the splash, the panel editor (a full-grid mode with no frame). **A message is a
modal; a hint is chrome.**

### The close affordance

Every modal surface carries a shared close control pinned to the frame's
top-right border, drawn and hit through one geometry function. Escape stays the
always-available dismiss, the close control its pointer twin, and both route to
that surface's one close path. **A mode's close control resolves to the mode's
own close target**: reusing the overlay's would close the wrong thing.

### The rule keys on what a surface *is*

Canopy has two ways to put a box over the dashboard: the closed overlay shell
(results, confirm, running, alert, notice) and **modes drawn as modals**,
stateful surfaces that rent the shared geometry and scrim but own their
reducers. Conventions written against the first let the second slip past them,
so the contract keys on what a surface *is*, not how it is implemented:

- **A modal surface** is anything drawn as a centred frame over the shared modal
  geometry — an overlay variant *or* a mode stage.
- **Every modal surface carries the close control, on every stage it can show.**
- **Parity is act-level and runs both ways.** Every act a surface's hints teach
  is reachable by click *on that surface* through some dedicated affordance — a
  row, a chip, a button, not necessarily the hint text — and every clickable
  affordance has a key path.
- **Hints derive per stage**, so a staged surface never advertises a key dead in
  the stage you are standing in.

The last has teeth: of the twenty-two hint contexts, several exist purely so a
key is taught where it is live — the detail column on an unadopted worktree
member (the only place the adopt key is bound), and a Project Settings modal
*with an operation in flight*, which answers nothing and so teaches nothing.

### Enforced structurally, not by memory

Three tests carry the contract. **Hint liveness** feeds every hinted key, in all
twenty-two contexts, to the reducer in a fixture standing there; it must change
observable state or return an action. **The modal inventory** is an exhaustive
match over overlay kinds and modes with the counts asserted, so adding either
stops the test compiling and forces a classification. **The close sweep** opens
each modal surface through its real entry, resolves the close control's cell
through the *pure hit-test*, asserts it is that surface's own close target,
activates it, and asserts the surface closed and left everything else alone. The
dispatch chains compile clean when a surface is forgotten — these hand-listed
fixtures are the only thing that goes red.

---

## 12.9 The theme

There is one built-in surface theme, a typed module rather than a configuration
file, resolving through the capability tiers at the backend boundary; draw code
targets the theme and never branches on capability. The colour system is the
**surface theme**; "palette" means the command surface.

| Tier | What survives |
|---|---|
| True colour | Everything, unchanged |
| 256 colour | Each channel quantized into the terminal's colour cube |
| Monochrome | No colour; bold, dim, reverse and underline survive |
| ASCII | No colour, and substituted glyphs |

The downgrade is **total** — every style resolves at every tier — and the
`NO_COLOR` guarantee is structural: forced ASCII produces an empty capability
set, an empty set is the ASCII tier, and that tier resolves every colour to the
terminal default. Load-bearing surfaces carry style-aware goldens at all four
tiers, so a colour regression fails a test rather than needing to be noticed.

### Five slots, and what a slot may never be

A profile can repaint five roles: **background** (surface fills), **border**
(idle panel and tile frames), **button** (button and key-hint chrome), **graph**
(the graph accent family) and **text** (the body foreground). Slots are
**roles**, and the boundary matters: meaning-bearing colours are never slots.
Health dots, severity, the gauge ramp, the token-class map, status dots, and the
selection and hover washes stay fixed in code. **A theme can restyle the
dashboard; it can never repaint meaning** — a red that means "error" must not be
a red somebody chose.

The theme handle is a small copyable value carrying five optional overrides over
the built-in constants, so with every slot unset each tier renders
byte-identically to the unthemed dashboard, pinned by the per-tier goldens. The
overrides ride the view snapshot like the version string and the layout, so
purity is preserved and draw code receives the theme as a parameter.

The picker offers ten named presets — red, blue, green, white, black, yellow,
pink, orange, cyan, purple — plus a reset per slot, bounding contrast risk to
ten known-legible choices; what is *stored* is the resolved `#RRGGBB` value, not
the preset name, so custom colours later would be a picker change rather than a
schema change. Selection applies live, in the same frame: in a terminal the
running dashboard is the only preview that tells the truth. The splash is exempt from
theming — it plays before any profile context exists.

---

## 12.10 The profile plane and its loader

Every piece of dashboard state outliving a session — project catalogue, feature
toggles, arrangements, profiles, membership, colours, startup pin — lives in one
machine-global, repository-independent store as **append-only events**, folded
by a latest-valid-per-key reduction.

There are eight tables, registered in the shared canonical-table registry, so
their append-only triggers, the immutability property test and the schema
snapshot are *generated, not hand-written*, and adding a table without its guard
turns the build red ([13-storage-and-schema.md](13-storage-and-schema.md)).

| Table | Folds to |
|---|---|
| project events | the machine-global project catalogue |
| toggle events | per-project feature intent |
| layout events | per-project arrangements (frozen for writes; read as a fallback) |
| profile events | which profiles exist, their names, which is active |
| profile-project events | one profile's ordered membership view |
| profile-layout events | arrangements per (profile, project) |
| profile-theme events | colour overrides per (profile, slot) |
| profile-startup events | the pinned launch profile (absent means follow last-selected) |

Every write is one of fifteen **insert-only** operations, asserted by an
exhaustive match in a test: adding an update or delete variant stops it
compiling. A removal is a superseding "removed" event, a reset a superseding
event carrying nothing. Nothing is ever clobbered.

### Fallbacks keep the past working

- The default profile is **seeded, not synthesized**: an empty log gets one
  "created" event named `default` and one "selected", so folds stay pure
  functions of the log with no sentinel id, and renaming it is an ordinary
  rename.
- The default profile with no membership events shows the **whole catalogue**,
  so a project registered from the command line keeps appearing; its first
  membership edit materializes the current view as explicit events.
- Creating a profile **copies the creating profile's effective project list**
  and diverges; duplicating clones the effective state whole — membership,
  arrangements, every resolved colour — and never writes a "selected" event,
  because **cloning is not switching**.
- With no profile-scoped arrangement, the default profile falls back to the
  older per-project layout table, so arrangements made before profiles existed
  survive. That table is frozen for writes and still guarded.

### The loader seam

The loop reads all of this through one injected trait: live profiles and the
active one, a profile's membership, a (profile, project) arrangement, colour
overrides, the startup pin, the global catalogue. Switching a profile emits
reload effects through that one seam, so the binary stays wiring and a test
drives the whole plane with a scripted double.

### Launch reads; it never writes

The pinned profile wins at launch when it is live, otherwise the last-selected
chain does, and either way **launch writes nothing**, which makes relaunch
idempotent by construction. The wrinkle: a pinned launch writes no "selected"
event, so the fold's active and the session's shown profile can differ. Two
rules keep the session coherent without a second notion of active. **Sticky
reloads** adopt the fold's active only when a switch landed in the same batch,
or when the shown profile is no longer live. **Deferred synchronization** makes
the session's *first write* prepend the synchronizing "selected" event, so
observation never mutates and the first mutation aligns the fold before anything
targeting "the active profile" acts on a stale one.

---

## 12.11 The splash director

The opening animation is a small state machine with three jobs. **Timing** goes
through an injected clock, so tests fast-forward without real sleeps and a skip
lands on the deterministic final frame; the sequence is hard-capped at five
seconds regardless of configuration, so a hostile preference cannot delay
startup. **Tiers** are a fidelity ladder — mesh raster, 2D Unicode, static
banner, skip — whose top rung needs a build carrying the 3D layer, because the
capability lattice never advertises mesh rasterization without it: the default
binary's ceiling is the 2D tier, a build-time fact. **Honesty** is a budget
ladder — each frame is measured against an eight millisecond budget, and a frame
that misses takes exactly one rung, halving raster density before dropping a
tier, never climbing back, so a borderline machine *settles*.

---

## 12.12 The read-only viewer plane

The Recall Viewer is a top-level surface, not a modal, and read-only *by
construction*: its whole state — pane focus, filter, expansion, selection,
scroll — is session-local with no persistence operation. Six structural
decisions make it trustworthy.

- **A distinct bus.** Launched directly it bypasses dashboard bootstrap: no
  watcher, no legacy import, no profile seeding, no project registration, no
  launch ingest, no write-capable feed. Existing database files open read-only;
  missing ones are *absence*, never created or migrated. Reusing normal startup
  and promising not to click anything that writes would already be false,
  because startup creates and appends.
- **The worker does everything expensive** — git access, database reads,
  reachability, transcript parsing, policy, blob reads, hunk assembly, syntax
  parsing — producing immutable display snapshots tagged by project and
  generation; the reducer and renderer do no input, output or parsing.
- **Shadow inspection is metadata-only.** The worker may enumerate local shadow
  reference names and tips and read commit metadata, but never opens a shadow
  tree or artifact: those may hold unredacted content, unacceptable as a source
  even for a surface that would redact them afterwards.
- **Transcript bytes are already-redacted durable artifacts** — the compact
  slice the checkpoint recorded by default, the stored redacted full transcript
  as fallback; malformed records are bounded, sanitized and reported without
  hiding the valid facts around them.
- **Diffs are reconstruction, not re-diffing.** Recorded hunk ranges plus the
  anchor's first-parent blobs are read through the blob seam, three context lines
  by default, gaps explicit. Policy controls code visibility, and an invalid or
  refused policy makes every file metadata-only; recomputing a diff would invent
  ranges and chronology the checkpoint did not record.
- **Syntax is semantic data, not a render dependency.** The parser maps scopes
  into Canopy-owned span classes, Canopy's theme chooses the colours, and a
  parse failure becomes plain sanitized text. No parser theme is used, keeping
  the ASCII and `NO_COLOR` tiers at parity.

Its command grammar is closed like the dashboard's and narrower: while the
viewer is active only viewer, select, focus, help and quit verbs plus
specifically audited read-only forms parse at all, and anything mutating,
serve-auditing, verification-appending or not proven zero-write is refused. One
consequence shows on screen: some information reads *unavailable*, or is
labelled repository-wide, rather than attributed to a checkpoint that did not
record it.

---

## Related chapters

[11-canopy-tour.md](11-canopy-tour.md), the surface all of this draws ·
[02-configuration.md](02-configuration.md) ·
[08-cost-and-savings.md](08-cost-and-savings.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) for the generated
append-only guards these tables inherit · [16-development.md](16-development.md)
· [17-invariants.md](17-invariants.md), whose standing rules the five properties
above are the dashboard's share.
