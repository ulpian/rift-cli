# 12. How Canopy is built

[11-canopy-tour.md](11-canopy-tour.md) walks the dashboard from outside; this is
the inside. You need it to change Rift (a panel, a key, a modal, a feed)
without breaking a property the toolchain leans on. Five do most of the work:

1. **The dashboard crate touches no storage**: it renders and reduces, never
   reading a database or spawning a process.
2. **Geometry is never stored**: clickable regions are recomputed from the solver
   that drew them.
3. **The loop is synchronous and idle-quiet.**
4. **Every act is reachable both ways**, by key and by pointer.
5. **Nothing the dashboard writes is overwritten**: its state is an append-only
   log, the ledger's discipline.

## 12.1 Three crates, one direction

`rift-canopy` (render seam, pure reducer, solvers, hit-test, theme, splash)
depends only on `rift-core` and `rift-registry`. `rift-canopy-data` (every
dashboard read and interface-state write) depends on it, never the reverse, so
the render side stays storage-free and the data plane is testable against
temporary-directory projections without the command line.
The reducer reaches the outside through six **seams**
(render backend, event source, command dispatcher, profile loader, settings
manager, clock), and tests inject scripted doubles, so
the dashboard is provable without a terminal.

---

## 12.2 The render seam

`RenderBackend` has two implementations: **the ASCII backend**, standard library
only and always compiled in, and **the engine backend** behind the `xre` feature.
**No module outside `render/` names an engine type**, so an engine change is a
one-directory change and the ASCII floor holds.

Selection reads a Canopy-owned capability bitset (a thin probe plus a pure
mapping, so tests never read a terminal); a build without the 3D layer cannot
*claim* mesh rasterization. `--ascii`, `NO_COLOR` or `RIFT_CANOPY_ASCII` empty the
set, and an empty set is the ASCII floor; selection degrades rather than fails,
even when the engine cannot initialize. On Windows the probe asks the console, since `COLORTERM` and `TERM` are never set there. The
shipped `rift` enables `tui`; the 3D layer is a second opt-in,
`tui-hires` (crate feature `xre-3d`).

---

## 12.3 One solver, two readers

The reducer is a pure function from a state and an event to quit or a list of
**effects** the loop performs. Rendering reads a **snapshot**; the hit-test is a
pure, engine-free function of it that recomputes geometry from the *same* solvers
the renderer calls and stores nothing.

> **Two solvers would eventually disagree.** If the renderer recorded rects as it
> drew and the hit-test read them back, a resize between draw and click puts the
> click one cell off. One solver, read twice, makes drawn cells and clickable
> cells the same cells by construction.

---

## 12.4 The synchronous loop

The crate has no async runtime: the engine's event queue is blocking, so a
runtime would add a thread for the same behaviour, and no load-bearing property
depends on it. A dispatch and a settings action run
in the loop body so their "running" frame appears **before** the blocking work.

The dashboard has no process-spawning seam, because **the strongest form of an
invariant is the one with no exceptions**: "spawns nothing" is verified in one
read, "spawns nothing except one property-tested seam" is not.
`crates/rift-canopy/tests/no_spawn.rs` scans the crate for the tokens that mean a
process could be born, and the only door to the command line is the injected
dispatcher, which takes *typed requests*, never strings. Interactive and headless launch share one startup path, because a live-data bug
hid on the path headless tests never took.

---

## 12.5 The animator

One contract reconciles idle quiet with animation. **State holds
progress** (no timers in the reducer); **a pure
advance-by-elapsed-time step moves it** and reports whether anything still
animates; **that report alone opens the fast-poll window** (33 ms while settling,
100 ms idle).

**The press flash is keyed by target, not by button**: a person activates one
control at a time, and a surface needing simultaneous flashes would need a keyed
map, which is where this design is revisited rather than bent. **A keyboard
activation arms the same pulse a click does**, the animation half of §12.8's
parity contract. Reduced motion and the ASCII floor show the final frame at once.

---

## 12.6 The data plane

The bus answers data requests with scoped, **read-only** projection queries on its
own thread. It has no write path, and failure degrades to an error status on a
card, not a panic.

**"Initialized" is the family's projection under the bus's state root**, never
`.rift/` in the checkout: `.rift/` is committed and travels with a clone, so
reading it would put a fresh clone in the initialized branch on every machine and
never offer setup. The settings projection reads the same file, so card and modal
cannot disagree. Both also ask whether the family's **binary of
record** (left by the git-hook install) names a binary still present; the
projection cannot stand in, since an agent hook creates one on first contact.
A view becomes a plan through **one** reading (`ProjectSetupPlan::from_view`),
shared by the page's `initialize` and the add flow's `Init`: one setup, not two
that drift.

**The watermark** is an **append cursor** plus a **rebuild epoch**. The epoch is
explicit because inferring a rebuild from a cursor moving *backwards* is unsound:
a rebuild landing *more* rows looks like ordinary ingest.

**Notification is a doorbell, not a delivery.** A native watcher carries **no
data**; an event schedules a watermark read and a fresh poll stays the only truth,
so a missed wake only delays and a duplicate or spurious one converges on output
the dirty gate drops. Without a watcher the bus polls alone. A second watch
covers every member's session-cache directory, since a family may run agents in
any member.

The poller runs at 250 ms after a change and 2 s when quiet; the status re-stat
runs every 600 idle ticks when watched but every 15 when poll-only, because with
no watcher the heartbeat *is* the wake.

**The channel cannot grow.** A **bounded** channel (sixty-four) with a staging
buffer keeping the **latest per (project, feed)** means a write storm neither
stalls the worker nor floods the loop. That works because **every feed emits full
section state, never a delta**: a dropped or duplicated delivery converges on the
fresh read.

**Scope and gauge.** Queries run against the project's own family databases, so
repositories never read each other's. `rift-nic-gauge` owns the pressure formula,
bands and model-to-window table, so drift ends by construction, and an unknown
model is explicit in the type, making a default window's "silent infinite
headroom" unrepresentable.

---

## 12.7 The layout is data

A panel editor reorders, moves and resizes, so the arrangement is a value: an
ordered list of columns, each a width weight and units (the rail, a card or a
panel) carrying ids, not indices, so a saved arrangement survives reordering. A
column's minimum is its widest unit, which yields the tier floors. Renderer and hit-test iterate the *same* solver output the only way §12.3's rule survives a movable layout.

The editor is a **mode** with its own reducer branch. The mouse drag is a *scoped exception* to ignore-drags, handled
**only** there with a drag live; grips and drop targets come from
the renderer's solver, since stored geometry would desync on a mid-drag resize. Editing is three-column only: narrow tiers restructure by *role*, where a
reordered model means nothing.

An arrangement is an **append-only event** per profile and project; a reset is a
superseding event carrying none (§12.10). A hand-written codec encodes it (the
render crate carries no serialization framework), and a malformed or
forward-version value decodes to nothing, so the canonical layout is the fallback. Retired card names are skipped as a fixed list; any other unknown token fails
the decode, since skipping unknowns would accept corruption.

---

## 12.8 Modal geometry and the parity contract

**The frame.** A modal is a centred box (80% each way, 95% on narrow tiers,
clear of the bars), computed by one function for draw and hit-test. The body stays
drawn behind it under a colour-only scrim, so monochrome and ASCII tiers drop the
dim and the border alone separates the modal.
**A message is a modal; a hint is
chrome**, and chrome is never modalized.

**The close affordance.** Every modal surface carries a shared close control on its
top-right border, through one geometry function. Escape and the control both reach that surface's one close path. **A mode's control resolves to the mode's own close target**; reusing the
overlay's would close the wrong thing.

**The rule keys on what a surface *is*.** The closed overlay shell (results,
confirm, running, alert, notice) and **modes drawn as modals** share the geometry
but not the reducer, and conventions written against the first let the second slip
past. So:

- **A modal surface** is anything drawn as a centred frame over the shared
  geometry, overlay or mode stage, and carries the close control on every stage.
- **Parity is act-level and runs both ways**: every act the hints teach is
  clickable *on that surface* through some dedicated affordance (not necessarily
  the hint text), and every clickable has a key.
- **Hints derive per stage**, so no stage advertises a dead key. Several of the
  twenty-two hint contexts exist only for this, such as a settings modal
  mid-operation, which answers nothing and so teaches nothing.

**Enforced structurally.** *Hint liveness* feeds every hinted key, in all
twenty-two contexts, to the reducer, which must act on it.
*The modal inventory* is an exhaustive match over overlay kinds and modes, so a new
one stops compiling until classified. *The close sweep* clicks each surface's close
control through the pure hit-test and asserts that surface, and nothing else,
closed. Dispatch chains compile clean when a surface is forgotten, so only
these fixtures go red.

---

## 12.9 The theme

One built-in surface theme, a typed module, resolves through the capability tiers
at the backend boundary; draw code never branches on capability. The downgrade to
256 colour, monochrome and ASCII is **total**, and the `NO_COLOR` guarantee is
structural: forced ASCII empties the capability set, an empty set is
the ASCII tier, and that tier resolves every colour to the terminal default.


A profile repaints five roles: **background**, **border**, **button**, **graph**
and **text**. Meaning-bearing colours (health, severity, the gauge ramp, token classes, selection) are never slots:
**a theme can restyle the dashboard; it can never repaint meaning**, since a red
that means "error" must not be a red somebody chose. With every slot unset the
output is byte-identical to the unthemed dashboard. Ten presets bound contrast risk; what is *stored* is the resolved `#RRGGBB`, not
the preset name, so custom colours would be a picker change, not a schema change.
The splash is exempt: it plays before any profile exists.

---

## 12.10 The profile plane and its loader

All dashboard state that outlives a session lives in one machine-global,
repository-independent store as **append-only events**, folded by a
latest-valid-per-key reduction, in eight tables: project, toggle, layout (frozen
for writes, read as a fallback), profile, profile-project, profile-layout,
profile-theme and profile-startup events. They are registered in the canonical-table
registry, so their append-only triggers, immutability property test and schema
snapshot are *generated, not hand-written* and a table without its guard turns the
build red ([13-storage-and-schema.md](13-storage-and-schema.md)). Every write is
one of fifteen **insert-only** operations, asserted by an exhaustive match; a
removal is a superseding "removed" event, a reset one carrying nothing.

The default profile is **seeded, not synthesized**, so folds stay pure functions
of the log. With no membership events it shows the **whole
catalogue**, so a project registered from the command line keeps appearing.
Duplicating a profile writes no "selected" event, because **cloning is not
switching**. Without a profile-scoped arrangement the default profile falls back
to the frozen layout table.

The loop reads all of it through one injected loader trait, so a test drives the
whole plane with a scripted double.

**Launch reads; it never writes**, so relaunch is idempotent by construction. A
pinned launch writes no "selected" event, so the fold's active and the shown
profile can differ; **sticky reloads** adopt the fold's active only when a switch
landed in the same batch or the shown profile is no longer live, and **late
synchronization** makes the session's *first write* prepend the "selected" event,
so observation never mutates and nothing acts on a stale "active profile".

---

## 12.11 The splash director

**Timing** goes through an injected clock, so tests fast-forward, and the sequence
is capped at five seconds so a hostile preference cannot delay startup. The top
**tier** (mesh raster) needs the 3D build, since the lattice never advertises it
without. **Honesty**: a frame over eight milliseconds drops one rung and never
climbs back, so a borderline machine *settles*.

---

## 12.12 The read-only viewer plane

The Recall Viewer is a top-level surface, not a modal, and read-only *by
construction*: its state is session-local with no persistence operation.

- **A distinct bus.** Launched directly it bypasses dashboard bootstrap (no
  watcher, import, seeding, registration, ingest or write-capable feed); databases
  open read-only and missing ones are *absence*, never created. Reusing normal
  startup while promising not to write would already be false, because startup
  creates and appends.
- **The worker does everything expensive** and hands over immutable snapshots;
  reducer and renderer do no input, output or parsing.
- **Nothing unredacted is read**: shadow inspection is metadata-only (never a
  shadow tree or artifact, which may hold unredacted content) and transcript bytes
  are already-redacted artifacts.
- **Diffs are reconstruction, not re-diffing**: recorded hunk ranges plus the
  anchor's first-parent blobs, gaps explicit. Recomputing would invent ranges and
  chronology the checkpoint did not record.
- **Syntax is data, not a render dependency**: Canopy's theme picks colours, so
  ASCII and `NO_COLOR` stay at parity.

Its command grammar is closed and narrower than the dashboard's: only viewer,
navigation and audited read-only forms parse; anything not proven zero-write is refused.

---

## Related chapters

[11-canopy-tour.md](11-canopy-tour.md) ·
[13-storage-and-schema.md](13-storage-and-schema.md) ·
[17-invariants.md](17-invariants.md), whose standing rules the five properties
above are the dashboard's share.
