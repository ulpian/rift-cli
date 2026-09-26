//! The source screens: tests that read the product half of this module's
//! own text (`mod.rs`) and hold its structural rules — one path derivation,
//! three record writers, sixteen refusal exits, no lossy transcription.
//! They live apart from the behavioural tests because they are tripwires on
//! spellings, and their docstrings say what each cannot see.

/// The **frame** every source screen below shares — and the assertion that
/// makes it a frame rather than an assumption.
///
/// Every source screen in this file asks its question of the *product half*:
/// everything above `mod tests`. Each of them used to derive that half for
/// itself — `source.split("\n#[cfg(test)]").next()` — five derivations of
/// one boundary whose precondition, that the anchor occurs **once**, was
/// written down nowhere.
///
/// **A second `#[cfg(test)]` at column zero moves all five horizons at
/// once**, and everything between it and `mod tests` is unscreened for
/// good. Measured rather than reasoned: `#[cfg(test)] mod probe {}`
/// planted above the real one, with a fresh `impl Walk` under it whose
/// body is the plainest call in the product file —
/// `self.refuse(rel, Refusal::NotAFile);` — ran **54 passed, 0 failed**.
/// Every screen green, the `mod` tripwire in
/// `only_three_places_in_this_module_write_a_record` included, because it
/// reads the same truncated half. No clever spelling was involved; the
/// frame did all the work. Re-planted against the assertion below, the
/// module runs **49 passed, 5 failed** — one failure per screen, each
/// naming this function.
///
/// So the boundary is derived **here and nowhere else**, and its
/// precondition rides with the derivation. That is the move the product
/// half already makes with [`resolve`], for the reason the screen below
/// gives: a second derivation is a second parse, and two parses drifting
/// apart is what every defect in this module has been. Ask this function
/// for the product half; do not split the source again.
///
/// The count is deliberately **stricter than the horizon needs** — an
/// anchor *below* `mod tests` cannot move `.next()` — because "exactly
/// one" is checkable at a glance where "the first one is the intended one"
/// is not, and the cost of the strictness is a red test with a message,
/// not a silent hole.
///
/// **The anchor is joined, not bare** (an earlier review found this used to
/// count `#[cfg(test)]` and never identify it). Counting the bare token
/// says there is one marker; it does not say the one marker is the one
/// above `mod tests`. A column-zero `#[cfg(test)]` on a helper `fn` plus
/// a respelled module attribute (`#[cfg(all(test))]`) keeps the count at
/// exactly one and moves the horizon up to the helper — every screen
/// green, everything between the decoy and the real module unscreened.
/// The joined anchor `\n#[cfg(test)]\nmod tests;` matches only the
/// attribute whose next line declares THE test module, so the decoy is not
/// an anchor and the respelling is not an anchor: count zero, refused,
/// named. Same shape `ci_meta::production_half` uses for `bins/`, and
/// assembled from pieces so this source cannot match its own spelling.
/// [`product_half_of`] is the pure form the calibration below drives
/// with synthetic sources; [`product_half`] is that applied to `mod.rs`,
/// the file that holds the product half.
fn product_half() -> &'static str {
    product_half_of(include_str!("../mod.rs"))
}

/// Everything above the ONE joined anchor in `source` — see
/// [`product_half`] for why the anchor is joined and the count is one.
fn product_half_of(source: &str) -> &str {
    // Assembled from pieces, as `ci_meta::production_half` is, so the anchor
    // is never spelled whole in a test file.
    let horizon = ["\n#[cfg(", "test)]\nmod tests;"].concat();
    assert_eq!(
        source.matches(horizon.as_str()).count(),
        1,
        "`mod.rs` must hold exactly one column-zero `#[cfg(test)]` whose \
         very next line declares `mod tests;` — because every source screen \
         here takes it as the end of the product half, and `.next()` \
         stops at the FIRST one. A second one (the fully ordinary \
         `#[cfg(test)] mod proptests;`) moves every horizon up at once and \
         leaves everything under it unscreened, which is how the plainest \
         `self.refuse(…)` in the file can sit in an `impl Walk` that no \
         screen reads; a respelled attribute (`#[cfg(all(test))]`) or a \
         decoy on a helper fn matches nothing and is refused here rather \
         than cut at. If you want a second test module, add it under \
         `tests/`, where it needs no second attribute; if you are \
         moving this boundary on purpose, say in the commit message which \
         screens you re-aimed and what now stands above them."
    );
    source.split(horizon.as_str()).next().unwrap_or_default()
}

/// The calibration for [`product_half_of`]: the decoy-plus-respelling
/// shape that kept the old bare-token count at exactly one is refused,
/// not cut at; the real shape returns the product half. The synthetic
/// sources are assembled from pieces so this test's own text carries no
/// column-zero anchor.
#[test]
fn a_column_zero_decoy_and_a_respelled_anchor_are_refused_not_cut_at() {
    let attr = ["#[cfg(", "test)]"].concat();
    let respelled = ["#[cfg(all(", "test))]"].concat();
    let decoyed = format!(
        "fn product() {{}}\n{attr}\nfn helper() {{}}\nfn unscreened() {{}}\n{respelled}\nmod tests;\n"
    );
    // The bare-token count is exactly one here — the old horizon would
    // have cut at the helper and left `unscreened` under it.
    assert_eq!(decoyed.matches(attr.as_str()).count(), 1);
    let refused = std::panic::catch_unwind(|| product_half_of(&decoyed).len());
    assert!(
        refused.is_err(),
        "a decoy attribute plus a respelled module anchor must be refused, not cut at"
    );

    let real = format!("fn product() {{}}\n{attr}\nfn helper() {{}}\n\n{attr}\nmod tests;\n");
    assert_eq!(
        product_half_of(&real),
        format!("fn product() {{}}\n{attr}\nfn helper() {{}}\n"),
        "the real shape cuts at the joined anchor and keeps the helper screened"
    );
}

/// The structural rule the three escapes cost, enforced rather than
/// promised: exactly one place in this module turns an entry into a path,
/// and every filesystem call takes a path that came from there. A second
/// derivation is a second parse, and two parses drifting apart is what
/// every defect here has been.
///
/// **This is a textual screen, and the doc used to overstate it.** It
/// counted `.join(` and claimed to "count the derivations and fail at two";
/// a second derivation written with `PathBuf::push` — that defect's shape exactly,
/// guard path ≠ opened path — sailed past it, and all 33 tests stayed
/// green. What it does now is hold an **inventory**: every product-half
/// line that builds a path or touches the filesystem is listed here
/// verbatim, so a new derivation shows up as a line this list does not
/// have. Measured against four second-derivations planted in `visit`,
/// each replacing the path `resolve` returned with a re-derived one:
/// `PathBuf::push` (that shape), `PathBuf::from(format!(..))` and
/// `Path::new(&s).to_path_buf()` are all **caught**. The list is also the
/// readable answer to "where does this module touch the disk?", which is
/// worth having on its own.
///
/// **What it still cannot see, measured rather than guessed:** a derivation
/// with no constructor token at all. `let full: PathBuf =
/// format!("{}/{}", work_tree.display(), rel).into();` compiles, re-derives
/// the opened path from `rel`, and this test stays **green**. So it screens
/// the spellings this module would plausibly use and does not prove there
/// are no others — an `Into`, an extension trait, a helper in another
/// module would each pass. Said plainly, with the counterexample, because
/// the previous wording ("counts the derivations and fails at two") is
/// exactly why a `push` derivation went unnoticed.
#[test]
fn only_one_place_in_this_module_turns_an_entry_into_a_path() {
    let product = product_half();
    let inventory: Vec<&str> = product
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| {
            l.contains(".join(")
                || l.contains("PathBuf::from")
                || l.contains("PathBuf::new")
                || l.contains("Path::new")
                || l.contains("to_path_buf")
                || l.contains("std::fs::")
                // A byte pushed onto the document is not a path derivation;
                // anything else pushed might be.
                || (l.contains(".push(") && !l.contains(".push(b'"))
        })
        .collect();
    assert_eq!(
        inventory,
        [
            // `resolve`, and only `resolve`, turns an entry into a path.
            ".join(rel)",
            "let mut full = work_tree.to_path_buf();",
            "full.push(name);",
            "&& std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_symlink())",
            // `normalize_entry` builds a String, never a path.
            "collapsed.push(c);",
            // Every other filesystem call takes a path `resolve` returned.
            "let Ok(meta) = std::fs::symlink_metadata(&path) else {",
            "std::fs::read_to_string(&path)",
            "let Ok(meta) = std::fs::symlink_metadata(&full) else {",
            "let Ok(target) = std::fs::read_link(full) else {",
            "let Ok(read) = std::fs::read_dir(full) else {",
            "let Ok(bytes) = std::fs::read(full) else {",
            // Not a derivation: it copies the path `resolve` already
            // returned, to use as a set key. Nothing is parsed and nothing
            // is opened — `full` arrives from the caller.
            "if self.recorded.insert(full.to_path_buf()) {",
        ],
        "the product half of this module must build paths in exactly one \
         place — `resolve`, which checks the components it builds from — \
         and every filesystem call must take a path it returned. If you \
         need a path here, ask `resolve` for it; if you need one it cannot \
         give you, change `resolve` so the check moves with it. If you \
         genuinely added a line that belongs, add it to this list in the \
         same commit and say in the message why it is not a second parse."
    );
}

/// The BOUND on the three record-writer screens, tripwired rather than
/// argued. All three read `mod.rs` and nothing else, while `Walk` being
/// private admits every file in this module's *subtree* — so a child module
/// in a second file writes records none of them can see. Planted end to end
/// rather than reasoned: `#[path = …] mod a18child;` whose body called the
/// ordinary `w.refuse(rel, …)` put `x d/a.a18child not-a-file` in the
/// document TWICE for one path — the double-record defect exactly, at 54
/// passed and 0 failed. This assertion does not close that hole — the child
/// is still invisible once it exists. It makes the one line that opens it
/// impossible to add quietly.
///
/// THE PREDICATE IS A WHITESPACE TOKEN, NOT A RUST LEXICAL ONE, and it is
/// written that way deliberately. Measured on this tree, it fires on three
/// benign shapes at once — `let _ = "a mod b";`, `/* mod a; */`, and a
/// trailing `// mod boundary note` — and each is FAIL-CLOSED and
/// self-describing: the failure prints the offending line, so a false
/// positive costs one glance and a reword, where a false negative costs a
/// module nobody screens. The file's own style keeps it far from that edge;
/// the product half carries no trailing `//`, no block comment and no
/// standalone `mod` word. What it does NOT see, also measured: a whole-line
/// `//` comment, filtered one line above the predicate, and `mod` welded to
/// an attribute — `#[allow(dead_code)]mod x {}` is legal Rust, is a single
/// whitespace token, and passed. That gap is closed by the fmt leg, which
/// splits the two onto separate lines; what the screens in this module do
/// and do not owe to `cargo fmt` is set out in
/// `every_refusal_exit_in_the_walk_is_enumerated_and_classified`.
#[test]
fn the_product_half_declares_no_child_module() {
    let product = product_half();
    let declared: Vec<&str> = product
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| l.split_whitespace().any(|t| t == "mod"))
        .collect();
    assert_eq!(
        declared,
        [] as [&str; 0],
        "the product half of this module declares no child module, and \
         the three screens holding `Walk`'s record writers are exhaustive \
         only while that is true. `mod tests` is the only child this \
         module has and it sits below the `#[cfg(test)]` split — \
         `product_half` pins that there is exactly one such split, which \
         is what makes `below it` mean anything at all. If you are adding \
         a module here, you \
         are adding a file these screens cannot read: give them that file \
         in the same commit, or say in the message why the record writers \
         stay in this one."
    );
}

/// The **warrant** for the enumeration next door, and it exists because the
/// enumeration shipped without it and the assumption had a hole.
///
/// Three places in this module write a record — [`Walk::link`]'s `l`,
/// [`Walk::file`]'s `f`, [`Walk::refuse`]'s `x` — and that is what makes
/// enumerating [`Walk::refuse`]'s callers *exhaustive* rather than merely
/// long: a line can only be a refusal exit by going through `refuse`.
///
/// **The hole, measured rather than argued.** The exit inventory filters
/// four call names and the window screen filters bare `refuse`, so a fourth
/// helper writing to `doc`/`records` **directly** — the idiom `link`
/// and `file` already use — matches neither. Planted in this module, it
/// produced `x d/x.a15vprobe not-a-file` **twice** for one path: two
/// records, two charges against [`MAX_ENTRIES`], the double-record defect
/// exactly, with
/// both of the other screens green. Re-measured on this tree: 53 passed,
/// 1 failed, and the one that failed was this one. **The total moves with
/// the module and the 1 does not** — it read 51 when this test was split
/// out of the enumeration, and it says nothing then or now except how many
/// tests the module happened to hold; the half of the measurement worth
/// keeping is that exactly one test noticed, and it was this one.
///
/// The filter takes reads as well as writes, and on **any** receiver rather
/// than on `self`, because separating them would be a guess about which
/// spellings mutate (`&w.doc` reads, `&mut w.doc` writes; a helper taking
/// `&mut Walk` writes `w.records`; a helper handing out `&mut self.doc`
/// writes neither) — and a guess about which spellings matter is precisely
/// what let that defect back in twice.
///
/// **What this cannot see**, in the shape its two siblings
/// (`only_one_place_in_this_module_turns_an_entry_into_a_path`,
/// `nothing_this_module_records_is_a_lossy_transcription`) already state:
/// it reads one module's product half by substring, so a spelling that
/// reaches these fields without naming them passes. **Demonstrated, not
/// supposed** — a helper destructuring `Walk { doc, records, .. }` and
/// writing through the bindings names neither field, and put
/// `x d/a.a18destr not-a-file` in the document twice for one path with
/// every screen in this module green. It also says nothing about any other
/// **file**, which is the honest bound: [`Walk`] is private to this
/// module, and private-to-module reaches the module's whole *subtree* —
/// `mod tests` below already constructs a [`Walk`] and writes its fields —
/// so a child module living in a separate file could write a record with
/// all three screens blind to it. That one is now **tripwired rather than
/// closed**, by `the_product_half_declares_no_child_module` above: the
/// child is still invisible once it exists, but the `mod` line admitting it
/// cannot be added quietly.
#[test]
fn only_three_places_in_this_module_write_a_record() {
    let product = product_half();
    let touches: Vec<&str> = product
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| l.contains(".doc") || l.contains(".records"))
        .collect();
    assert_eq!(
        touches,
        [
            // ---- `capture_document`: reads the finished document. Writes
            // nothing, and the header it prepends goes onto `out`.
            "if w.doc.is_empty() {",
            "let mut out = Vec::with_capacity(w.doc.len() + CAPTURE_DOC_HEADER.len() + 1);",
            "out.extend_from_slice(&w.doc);",
            // ---- WRITER 1 of 3: `link`'s `l` capture.
            "self.records += 1;",
            "self.doc.extend_from_slice(b\"l \");",
            "self.doc.extend_from_slice(record.as_bytes());",
            "self.doc.push(b'\\n');",
            // ---- WRITER 2 of 3: `file`'s `f` capture, header then content
            // lines.
            "self.records += 1;",
            "self.doc.extend_from_slice(b\"f \");",
            "self.doc.extend_from_slice(rel.as_bytes());",
            "self.doc.push(b'\\n');",
            "self.doc.push(b' ');",
            "self.doc.extend_from_slice(line.as_bytes());",
            "self.doc.push(b'\\n');",
            // ---- `cap_spent`: reads the count to decide, writes via
            // `exhausted` -> `refuse`.
            "if self.records >= MAX_ENTRIES {",
            // ---- WRITER 3 of 3: `refuse`'s `x` record — the one every
            // refusal exit in the enumeration next door goes through.
            "self.records += 1;",
            "self.doc.extend_from_slice(b\"x \");",
            "self.doc.extend_from_slice(safe.as_bytes());",
            "self.doc.push(b' ');",
            "self.doc.extend_from_slice(why.as_str().as_bytes());",
            "self.doc.push(b'\\n');",
        ],
        "three places in this module write a record — `link`'s `l`, \
         `file`'s `f`, `refuse`'s `x` — and \
         `every_refusal_exit_in_the_walk_is_enumerated_and_classified` is \
         exhaustive only because of it: a refusal is a refusal by going \
         through `refuse`. A fourth writer is a fourth exit that the \
         enumeration's call-name filter cannot see, which is \
         how one path earned two records and two charges against \
         `MAX_ENTRIES` three times running. If you are adding one, do not: \
         route it through `refuse` (or `refuse_once`, if it decides after \
         `resolve` produced a path). If you genuinely need a fourth, it \
         goes in this list, in that enumeration, and in `Walk::recorded`'s \
         own, in the same commit."
    );
}

/// The exit inventory, and it exists because a **counted** claim now sits
/// on [`Walk::recorded`]: sixteen refusal exits in this walk write a
/// record, eleven keyed and five not. This is what fails if that stops
/// being true.
///
/// It is here because the claim it replaced was general — "the insert runs
/// for every non-directory path, before the decision is made" — and general
/// was the whole defect. It was written from the rule set, it was false at
/// one exit, and it was corrected **three times** by adding a shape to
/// `a_path_named_twice_is_captured_once` while leaving the reason general
/// enough to cover the next exit nobody had checked. A behavioural shape
/// catches the exit its author thought of. An inventory catches the one they
/// did not: a seventeenth exit, or a moved one, reddens here until it is
/// listed **and classified**, in the same commit.
///
/// **Three screens hold this property and they live in three tests.**
/// `only_three_places_in_this_module_write_a_record` is their WARRANT: it
/// pins the fact that makes the enumeration below *exhaustive* rather than
/// merely long — a line can only be an exit by going through
/// [`Walk::refuse`], so enumerating that function's callers enumerates the
/// refusals. That was an unstated assumption for a round, and it had a hole
/// — read its docstring for what the hole was and what it produced.
/// `the_window_from_resolving_a_path_to_keying_it_takes_refuse_once_only`
/// holds the second, which is structural rather than textual; it was split
/// out of this test when the two together crossed clippy's line ceiling,
/// which is this module's idiom for that lint rather than an `#[allow]`.
///
/// This one is the **inventory**: every line that writes a record or spends
/// the record cap, in source order, each labelled with whether the path it
/// names is keyed — the three helper bodies included, so that rerouting an
/// exit through a different helper is visible too.
/// **What this cannot see**, said plainly for the reason its siblings say
/// it: the keyed/unkeyed labels are asserted *text*, not derived facts.
/// This list is textual and the window screen next door is structural, so a
/// reader who mislabels a line here fails no test — a verifier planted
/// exactly that (an exit listed, but labelled KEYED when it is not) and
/// only the window named it.
///
/// **What nobody can do is *move* or *delete* an exit silently** — though
/// not both by the same screen, and the attribution is worth getting right
/// because it says which test to read when one goes red. *Delete* is the
/// inventory's: the list is exact, so a line that goes away shortens it.
/// *Move* is the inventory's **only when the move reorders the list**, and
/// a move exists that does not: hoisting the `NotAFile` exit above
/// `self.recorded.insert(` leaves the nineteen filtered lines
/// **byte-identical**, because the insert is not one of them. Measured
/// rather than reasoned — the inventory stayed green and the window alone
/// went red, 53 passed and 1 failed. That is the division of labour working
/// as built: the inventory holds the list, the window holds the interval,
/// and a move *into* the interval is the window's to catch.
///
/// ***add* is a tripwire, not a proof**, and it is written that way because
/// the stronger sentence kept measuring false. It stood here as "a new exit
/// either calls one of the four names the inventory filters or writes
/// `doc`/`records` itself, and this module holds no third way to put a line
/// in the document" — **contradicted by its own sibling one docstring away**
/// (`only_three_places_in_this_module_write_a_record` states plainly that "a
/// spelling that reaches them without naming them would pass"), and then
/// defeated in measurement repeatedly — most recently by the two
/// path-qualified spellings below — each time producing two records
/// for one path with every screen green.
///
/// What the two screens between them catch: a call to one of the four
/// names — **on any receiver and under any path qualification** — and a
/// direct write to `doc`/`records`. The qualification half was a defeat and
/// then its repair was a second one. `Walk::refuse(self, …)` is neither
/// `.refuse(` nor a field name, and passed both screens while putting
/// `x d/a.a18probe not-a-file` in the document twice for one path at 54
/// passed and 0 failed. Adding `Walk::refuse(` reddens that plant on both
/// screens at 52 passed and 2 failed — and is then defeated in turn by
/// `Self::refuse(self, …)`, the same call one spelling over, back to 54
/// passed and 0 failed on the widened filters. So both filters anchor on
/// `::refuse(` rather than on a type name, which is what makes `Self::`,
/// `<Walk>::` and a fully-qualified path one case instead of three. The
/// widening is pure: no line in the product half carries any of those
/// spellings, so the nineteen-line list is unchanged and green on a clean
/// tree.
///
/// **This screen and the window next door lean on the fmt leg, and that is
/// said here rather than left implied.** Every anchor above is a substring
/// of a *single line*, so whitespace beats them at source level with no
/// exotic spelling at all: `Walk::refuse (self, …)` with one space carries
/// no `::refuse(`, and the same call broken after the `::` carries it on
/// neither line. Both were planted in the product half and both left this
/// test and the window **green** — 54 passed, 0 failed. What stops them is
/// `cargo fmt --all -- --check`, the first leg of `./skills.sh check`:
/// measured on this tree, rustfmt deletes the space and rejoins the split
/// path, so neither shape survives on a tree the gate accepts.
///
/// **Why lean rather than widen**, when the house fixed the matcher in the
/// analogous case — `xtask/src/ci_meta/lints.rs`,
/// `catches_whitespace_before_the_paren`, "`#![allow (clippy::all)]` is
/// legal Rust and must not evade the gate". The precedent is *not* that
/// fmt cannot see the shape: rustfmt normalises `#[allow (dead_code)]`
/// too, measured in the same run as the two above. It is that a gate of
/// last resort must not depend on another gate — and `ci_meta` is that
/// gate, screening the whole workspace's escape hatch from the lint system
/// across files no single author owns. These five screens are one module's
/// own tests, run by the same command as fmt, and they sit on a ladder
/// with **no top**: a macro expanding to `w.$m(` reaches [`Walk::refuse`]
/// with no such spelling anywhere in the source, and the frame defeat that
/// [`product_half`] now pins beat all five at once *without any spelling
/// at all*. One more rung would buy one shape and cost a second
/// normalising parse of the source, in a module whose every defect has
/// been two parses drifting apart.
///
/// **The gap that reliance leaves, named and measured rather than
/// hoped away:** `#[rustfmt::skip]` on the offending item opts out of the
/// fmt leg locally — planted with the spaced call under it, `cargo fmt
/// --all -- --check` exits **0** and this module runs **54 passed, 0
/// failed** — and a `cargo test` run with no fmt leg at all sees none of
/// this. Both are one-line, diff-visible additions, which is the same
/// standing every other tripwire in this module has.
///
/// **Among the cases none of these covers** — and the enumeration is open,
/// because these are the routes that were *planted*, not the routes that
/// exist. **(i) A spelling that reaches the four functions or the two
/// fields without naming either.** Two were planted: a helper
/// destructuring `Walk { doc, records, .. }` and writing through the
/// bindings, and an alias `let f: fn(&mut Walk, &str, Refusal) =
/// Walk::refuse;` called as `f(w, …)`. Each earned two records for one
/// path, and with both live the module ran 54 passed, 0 failed.
/// **(ii) A child module in a separate file.** [`Walk`] being private
/// bounds the writers to this module's own *subtree*, not to `mod.rs`,
/// and all three screens read `mod.rs` alone: `#[path = …] mod
/// a18child;` whose body called the ordinary `w.refuse(rel, …)` was
/// planted and produced the same two records with all three green. Its
/// *declaration* is now tripwired in
/// `only_three_places_in_this_module_write_a_record` — re-planted against
/// that assertion the module runs 53 passed, 1 failed, and the one failure
/// is the tripwire. A tripwire and not a closure: the child's writes stay
/// invisible, only the line admitting them does not.
///
/// The labels themselves are pinned behaviourally by
/// `a_path_named_twice_is_captured_once` and its four siblings (spanning
/// both keying sites) and by
/// `a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming`.
#[test]
fn every_refusal_exit_in_the_walk_is_enumerated_and_classified() {
    let exits = refusal_exit_lines(product_half());
    assert_eq!(
        exits,
        [
            // ---- `visit`: five exits, and the boundary runs through it.
            // UNKEYED — the record cap is charged before `resolve` runs.
            "if self.cap_spent(rel) {",
            // UNKEYED — `hard_refusal` decides on the spelling alone.
            "self.refuse(rel, why);",
            // UNKEYED — `resolve` returned no path to key on.
            "self.refuse(rel, why);",
            // KEYED, by the helper — this exit returns three lines before
            // the insert, which is the whole of the double-record defect.
            "self.refuse_once(&full, rel, Refusal::Missing);",
            // KEYED, by the insert immediately above it.
            "self.refuse(rel, Refusal::NotAFile);",
            // ---- `link`: three exits, every one past that insert.
            // KEYED.
            "self.refuse(rel, Refusal::Missing);",
            // KEYED.
            "self.refuse(rel, Refusal::UnsafeLinkTarget);",
            // KEYED.
            "self.refuse(rel, Refusal::UnsafeLinkTarget);",
            // ---- `walk_dir`: four exits. A directory never reaches the
            // insert, so the two records it can earn key here instead.
            // KEYED, by the helper.
            "self.refuse_once(full, rel, Refusal::TooDeep);",
            // KEYED, by the helper.
            "self.refuse_once(full, rel, Refusal::Missing);",
            // UNKEYED — the cap is charged on a summary, which is no path.
            "if self.cap_spent(&summary) {",
            // UNKEYED — the summary is not a path by construction.
            "self.refuse(&summary, Refusal::UnsafePath);",
            // ---- `file`: four exits, every one past that insert.
            // KEYED.
            "self.refuse(rel, Refusal::TooLarge);",
            // KEYED.
            "self.exhausted(rel, Refusal::BudgetSpent);",
            // KEYED.
            "self.refuse(rel, Refusal::Missing);",
            // KEYED.
            "self.refuse(rel, Refusal::Binary);",
            // ---- Not exits: the three helper bodies the sixteen above go
            // through — `exhausted`, `cap_spent` and `refuse_once`.
            "self.refuse(rel, why);",
            "self.exhausted(rel, Refusal::TooMany);",
            "self.refuse(rel, why);",
        ],
        "sixteen refusal exits write a record and three helpers carry them \
         If you added an exit, put it in this list, label it keyed or \
         unkeyed, and make `Walk::recorded`'s enumeration say the same \
         thing — in this commit. If it decides after `resolve` has produced \
         a path, it is `refuse_once` and not `refuse`; that mistake is \
         the double-record defect and has now been made three times."
    );
}

/// Every line of the product half that calls one of the four names an exit
/// goes through — trimmed, comments dropped, in source order.
///
/// On ANY receiver, not on `self`: a helper taking `&mut Walk` and calling
/// `w.refuse(rel, …)` is an exit like any other, and it is invisible to a
/// `self.`-anchored filter. That gap was measured — the probe put
/// `x d/x.a16vprobe not-a-file` in the document twice, with all three
/// screens green.
///
/// And under ANY path qualification, which is the `::` half:
/// `Walk::refuse(self, …)` is that same call spelled without a receiver, and
/// it defeated the line above. Anchoring on `::refuse(` rather than on
/// `Walk::refuse(` is deliberate and measured too — the type-named form was
/// defeated in turn by `Self::refuse(self, …)`, one spelling over, while `::`
/// takes `Self::`, `<Walk>::` and a fully-qualified path alike.
fn refusal_exit_lines(product: &str) -> Vec<&str> {
    product
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| {
            l.contains(".refuse(")
                || l.contains(".refuse_once(")
                || l.contains(".exhausted(")
                || l.contains(".cap_spent(")
                || l.contains("::refuse(")
                || l.contains("::refuse_once(")
                || l.contains("::exhausted(")
                || l.contains("::cap_spent(")
        })
        .collect()
}

/// The window screen below opens PAST the `match` that decides the path,
/// because `resolve`'s `Err` arm is the one bare `refuse` that belongs
/// inside the interval. That excuse is positional, not semantic: the skip
/// steps over the `Ok` arm as well, and the `Ok` arm is already in the true
/// interval — the path is resolved there and not yet keyed. Measured, not
/// supposed: a bare `refuse` planted in the `Ok` arm, then listed and
/// labelled in the inventory above exactly as its own failure message
/// instructs, left the whole suite GREEN while one path earned two records
/// and two charges against `MAX_ENTRIES`.
///
/// So the skipped region is pinned line for line. The `Ok` arm hands the
/// resolved path straight out and holds no code at all, which is what makes
/// skipping it safe; anything else in there — including a `refuse` —
/// reddens here, and this assertion is not vacuous under a moved anchor
/// either (a start that stops matching empties the list, an end that stops
/// matching runs it to the end of `visit`, and both fail).
#[test]
fn the_match_that_decides_the_path_holds_no_code_in_its_ok_arm() {
    assert_eq!(
        decision_lines(visit_body()),
        [
            "let full = match resolve(work_tree, rel) {",
            "Ok(full) => full,",
            "Err(why) => {",
            "self.refuse(rel, why);",
            "return;",
            "}",
        ],
        "the window screen skips this whole `match` in order to \
         excuse the one bare `refuse` that belongs in it — `resolve`'s \
         `Err` arm, which has no path to key on. That skip is only \
         honest while the `Ok` arm is `Ok(full) => full,` and nothing \
         else: a decision made there is made after the path exists and \
         before the walk keys it, which is the double-record defect \
         exactly. If you need code \
         in that arm, the window must start before the `match` and the \
         `Err` arm must be excused by name instead."
    );
}

/// The **window** screen is the defect's own shape rather than a list:
/// between the line where [`resolve`] hands back a path and the line
/// where [`Walk::visit`] keys it, a path *exists* and is *not yet keyed*,
/// so any bare `refuse` in that interval duplicates by construction. The
/// double-record defect was exactly one such line. The interval must therefore hold no bare
/// `refuse` at all, **on any receiver** — only [`Walk::refuse_once`],
/// which keys what it names. This filter and the inventory's next door both
/// take `.refuse(` rather than `self.refuse(` for that reason: a helper
/// taking `&mut Walk` and calling `w.refuse(rel, …)` is an exit, it was
/// planted, and it put two records in the document with every screen
/// green. Both also take `::refuse(`, for the same reason one spelling
/// further out: `Walk::refuse(self, …)` is that call with no receiver at
/// all, it was planted in this very interval, and it went green on both
/// screens while writing `x d/a.a18probe not-a-file` twice for one path.
/// The anchor is `::refuse(` and not `Walk::refuse(` because the type-named
/// form was itself defeated by `Self::refuse(self, …)`. Both anchors are
/// single-line substrings and are therefore beaten by whitespace at source
/// level; that this screen leans on `cargo fmt` for those shapes, what the
/// house precedent actually says about it, and what the reliance leaves
/// open are set out with their measurements in
/// `every_refusal_exit_in_the_walk_is_enumerated_and_classified`.
/// Note what the window deliberately starts after: [`resolve`]'s
/// own `Err` arm is a bare `refuse` and belongs there, because that exit
/// has no path to key. **That excuse is positional, so it excused more
/// than it meant to**: skipping to the `match`'s closing `};` skips the
/// `Ok` arm too, and the `Ok` arm is already inside the interval — the
/// path is resolved there and not yet keyed. A bare `refuse` planted in
/// it, then listed and labelled in the inventory next door exactly as that
/// assertion's own failure message instructs, left the whole suite green
/// with two records for one path. So the skipped `match` is pinned
/// line for line by `the_match_that_decides_the_path_holds_no_code_in_its_ok_arm`
/// before the window runs: the `Ok` arm is
/// `Ok(full) => full,` and holds no code, which is what makes skipping
/// it safe. **A window bounded by source literals is empty
/// whenever an anchor stops matching, and an empty window screens
/// nothing while still passing** — so the window is required to
/// contain the `lstat`, the one syscall the interval exists around,
/// before it is required to hold no bare `refuse`. That anchor is
/// deliberately not an exit: anchoring on that defect's own `refuse_once` would
/// confound "the window drifted" with "the exit changed", which is
/// the inventory's job. The hole was measured, not imagined: hoisting
/// the call out of its `match`, restoring the bare `refuse` in it and
/// relaxing the list to match left this whole test green, and only
/// `a_path_whose_lstat_fails_is_keyed_by_its_refusal` went red.
#[test]
fn the_window_from_resolving_a_path_to_keying_it_takes_refuse_once_only() {
    let window = keying_window(visit_body());
    assert!(
        window.contains(&"let Ok(meta) = std::fs::symlink_metadata(&full) else {"),
        "the window screen is not looking at the interval it screens. It \
         runs from the line calling `resolve(work_tree, rel)`, past the \
         `}};` that closes that decision, to the `self.recorded.insert(` \
         that keys the path — and if any of those three anchors has moved \
         or been respelled the window is empty and the assertion below \
         passes vacuously. What it saw instead: {window:?}"
    );
    let unkeyed_window: Vec<&str> = window
        .iter()
        .copied()
        .filter(|l| l.contains(".refuse(") || l.contains("::refuse("))
        .collect();
    assert_eq!(
        unkeyed_window,
        [] as [&str; 0],
        "between the path being resolved and the path being keyed, a bare \
         `refuse` writes a record for a path the walk never keys — so a \
         second spelling writes it again and charges `MAX_ENTRIES` twice. \
         That interval takes `refuse_once` only."
    );
}

/// The text of [`Walk::visit`] from its signature on — the function both
/// window screens read.
fn visit_body() -> &'static str {
    product_half()
        .split("fn visit(&mut self, work_tree: &Path, rel: &str, depth: usize) {")
        .nth(1)
        .unwrap()
}

/// The `match` that decides the path: from the `resolve` call to the `};`
/// that closes it, trimmed, comments dropped.
fn decision_lines(visit: &str) -> Vec<&str> {
    visit
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.contains("resolve(work_tree, rel)"))
        .take_while(|l| *l != "};")
        .filter(|l| !l.starts_with("//"))
        .collect()
}

/// The interval between the path being decided and the path being keyed:
/// past the `match`'s own closing brace (its `Err` arm refuses a path that
/// does not exist, which is the one bare `refuse` that belongs), up to the
/// `self.recorded.insert(` that keys the path.
///
/// The window is bounded by source literals, so it is EMPTY whenever an
/// anchor stops matching — and an empty window satisfies the caller's
/// assertion without screening anything. Measured, not supposed: hoisting
/// the call out of the `match` (`let decided = resolve(…); let full = match
/// decided {`), restoring that defect's bare `refuse` in the interval and
/// relaxing the inventory to match left the window test GREEN, and only the
/// behavioural `a_path_whose_lstat_fails_is_keyed_by_its_refusal` went red.
/// A refactor that is not about exits at all removed the structural floor
/// with nothing to say so.
///
/// So the window must contain the thing the interval exists FOR. That is
/// the `lstat` — the one syscall between the path being resolved and the
/// path being keyed, and the reason a decision can be made in there at all.
/// It is deliberately not an exit: anchoring on that defect's own
/// `refuse_once` would make the assertion fire whenever that exit moved,
/// which is the inventory's job, and would confound "the window drifted"
/// with "the exit changed".
fn keying_window(visit: &str) -> Vec<&str> {
    visit
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.contains("resolve(work_tree, rel)"))
        .skip_while(|l| *l != "};")
        .take_while(|l| !l.contains("self.recorded.insert("))
        .filter(|l| !l.starts_with("//"))
        .collect()
}

/// The sibling inventory: **no byte string becomes a record by being
/// approximated.** Two of the three sites below were `to_string_lossy`
/// calls that shipped in this delta's own first commit and were found by a
/// verifier rather than by a test — a symlink target recorded as a link the
/// filesystem does not hold, and a directory entry recorded as a path the
/// kernel does not have. Neither was visible in a diff review, because a
/// lossy conversion is the *shortest* way to write each line.
///
/// So the module's own rule ([`Refusal::Binary`]: capturing binary bytes
/// would "store a lossy transcription while claiming to hold the file") is
/// pinned structurally, in the shape this module already uses for its path
/// derivations. Adding a fourth site is a test edit, and a test edit is a
/// sentence in a commit message.
///
/// The survivor in [`resolve`] is listed rather than removed because it
/// cannot lose anything: it feeds two *comparisons* and never the path,
/// which is pushed from the original component.
///
/// **What this screen missed, and what it still cannot see.** It watched
/// `_lossy` and `.to_str()`, which is two of the spellings; `Path::display`
/// is a third and passed unseen, and the counterexample is live in this
/// crate rather than hypothetical — `provision.rs:60` writes
/// `report.install_root.display().to_string()` into the durable
/// `substrate_install` row, so a non-UTF-8 install root would be stored as
/// a path the filesystem does not hold. That is Rift's own path and low
/// severity, it is outside this item, and it is named here with its file
/// and line so the next reader does not have to find it twice. The screen
/// below now takes `.display()` too, so a fourth site cannot appear
/// *in this module* by that spelling.
///
/// **The site outside this module is not the only one, and it is the only
/// durable one.** `brand_free` in `parity.rs` —
/// a case-insensitive search for the retired token (assembled from pieces) —
/// is the same spelling in the benign class: it feeds a boolean predicate
/// and never a record, exactly like the [`resolve`] survivor above, so an
/// approximation there can change an answer but cannot become a stored
/// path. Named because the paragraph above, alone, reads as "the one thing
/// this screen cannot see is a durable write", and the honest shape of the
/// blind spot is that it cannot see either class.
///
/// The blind spot is stated rather than closed, in the shape the sibling
/// test `only_one_place_in_this_module_turns_an_entry_into_a_path` already
/// uses: this reads one module's product half by substring, so a conversion
/// spelled some other way (`String::from_utf8_lossy` on a slice held
/// elsewhere, `OsStr::to_owned` then a `Display` impl, a helper in another
/// module) passes it, and it says nothing at all about the rest of the
/// crate. It is a tripwire on the shortest ways to write the mistake, not a
/// proof that the mistake is unwritable.
#[test]
fn nothing_this_module_records_is_a_lossy_transcription() {
    let product = product_half();
    let inventory: Vec<&str> = product
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| l.contains("_lossy") || l.contains(".to_str()") || l.contains(".display()"))
        .collect();
    assert_eq!(
        inventory,
        [
            // Compared, never pushed: `resolve` builds the path from the
            // original component, and `.git` is four ASCII bytes.
            "let text = name.to_string_lossy();",
            // A discovered name the document cannot spell: refused.
            "match name.to_str() {",
            // A symlink target the document cannot spell: refused.
            "let Some(target) = target.to_str().map(str::to_owned) else {",
        ],
        "every byte string this module turns into text must either be \
         refused when the conversion would lose something, or provably \
         feed nothing but a comparison. A new `to_string_lossy` or \
         `.display()` here writes a record that reads back perfectly and \
         names something the filesystem does not hold — which is what \
         `Refusal::Binary` already refuses for file content, and is the \
         rule for every field, not just that one. This screen is a \
         tripwire on three spellings in one module, not a proof: see its \
         docstring for what it cannot see, including a live `.display()` \
         durable write elsewhere in this crate."
    );
}
