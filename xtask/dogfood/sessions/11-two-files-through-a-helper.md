<!-- intent: read two files, hand the same two to a subagent, and reconcile the two readings -->
Work in this repository. Use read-only tools for every step below.

1. Read `crates/rift-core/src/id.rs`.
2. Read `crates/rift-core/src/identity.rs`.
3. Using the Agent tool, start one subagent and give it this task, naming
   both paths in its prompt exactly as written here: read
   `crates/rift-core/src/id.rs` and `crates/rift-core/src/identity.rs`, and
   report in five lines which identifier types `identity.rs` builds on from
   `id.rs`, with the line numbers where each is used.
4. When the subagent reports back, compare its five lines against your own
   reading of the two files and say where the two agree and where they
   differ.
5. Write a five-line summary of how an identity is derived from an id.

Do not modify any file, and do not run `git`.
