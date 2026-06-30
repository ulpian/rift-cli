//! Workspace meta-checks — the topology, toolchain, and inheritance guards that
//! the build plan (Stage 1 · Phase 1 · Step 1/2) mandates.
//!
//! These parse the static workspace manifests (no `cargo metadata` subprocess)
//! and return a list of violations; the `#[test]`s assert the list is empty, so
//! `cargo test --workspace` fails if the crate graph drifts:
//! * the dependency graph is acyclic,
//! * no member except `rift-store` depends on a storage backend,
//! * `[workspace].members` matches the crate/bin directories on disk,
//! * every member opts into the workspace lints + package inheritance,
//! * `rust-toolchain.toml` pins an exact channel with the required components,
//! * `[workspace.package]` declares the expected MSRV + edition.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml::Value;

pub mod coverage;

/// A fallible meta-check: `Ok(violations)` (empty ⇒ healthy) or a hard `Err`
/// (a manifest could not be read or parsed).
pub type Check = Result<Vec<String>, String>;

/// The workspace root — the parent of this crate's manifest directory.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn read_toml(path: &Path) -> Result<Value, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    text.parse::<Value>()
        .map_err(|e| format!("parse {}: {e}", path.display()))
}

/// The relative member paths declared in `[workspace].members`.
fn declared_members(root: &Path) -> Result<Vec<String>, String> {
    let manifest = read_toml(&root.join("Cargo.toml"))?;
    let members = manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(Value::as_array)
        .ok_or_else(|| "root Cargo.toml has no [workspace].members array".to_string())?;
    Ok(members
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect())
}

/// `[package].name` declared in a member manifest.
fn package_name(manifest: &Value) -> Option<String> {
    manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The intra-workspace dependency keys of `manifest` (normal + build deps only;
/// dev-dependency cycles are legal and excluded), restricted to `names`.
fn workspace_dep_edges(manifest: &Value, names: &BTreeSet<String>) -> Vec<String> {
    let mut edges = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        if let Some(tbl) = manifest.get(section).and_then(Value::as_table) {
            for key in tbl.keys() {
                if names.contains(key) {
                    edges.push(key.clone());
                }
            }
        }
    }
    edges
}

/// Map of member package name → its in-workspace dependency edges.
fn dependency_graph(root: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let members = declared_members(root)?;
    let mut manifests = Vec::new();
    let mut names = BTreeSet::new();
    for m in &members {
        let manifest = read_toml(&root.join(m).join("Cargo.toml"))?;
        if let Some(n) = package_name(&manifest) {
            names.insert(n.clone());
            manifests.push((n, manifest));
        }
    }
    let mut graph = BTreeMap::new();
    for (name, manifest) in &manifests {
        graph.insert(name.clone(), workspace_dep_edges(manifest, &names));
    }
    Ok(graph)
}

/// Depth-first cycle detection over the in-workspace dependency graph.
fn find_cycle(graph: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn dfs<'a>(
        node: &'a str,
        graph: &'a BTreeMap<String, Vec<String>>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        marks.insert(node, Mark::Visiting);
        stack.push(node);
        for dep in graph.get(node).into_iter().flatten() {
            match marks.get(dep.as_str()) {
                Some(Mark::Visiting) => {
                    // Found a back-edge — report the cycle from `dep` onward.
                    let start = stack.iter().position(|n| *n == dep.as_str()).unwrap_or(0);
                    let mut cycle: Vec<String> = stack
                        .get(start..)
                        .unwrap_or(&[])
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect();
                    cycle.push(dep.clone());
                    return Some(cycle);
                }
                Some(Mark::Done) => {}
                None => {
                    if let Some(found) = dfs(dep, graph, marks, stack) {
                        return Some(found);
                    }
                }
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
        None
    }
    let mut marks = BTreeMap::new();
    for node in graph.keys() {
        if !marks.contains_key(node.as_str()) {
            if let Some(cycle) = dfs(node, graph, &mut marks, &mut Vec::new()) {
                return Some(cycle);
            }
        }
    }
    None
}

/// The dependency graph rooted at `rift-core` must be acyclic.
///
/// # Errors
/// If a manifest cannot be read/parsed.
pub fn check_acyclic(root: &Path) -> Check {
    let graph = dependency_graph(root)?;
    Ok(find_cycle(&graph)
        .map(|c| vec![format!("dependency cycle: {}", c.join(" -> "))])
        .unwrap_or_default())
}

/// Only `rift-store` may depend on a storage backend (`rusqlite` today; `turso`
/// is the production target behind the same seam).
///
/// # Errors
/// If a manifest cannot be read/parsed.
pub fn check_storage_backend_isolated(root: &Path) -> Check {
    let mut violations = Vec::new();
    for m in declared_members(root)? {
        let manifest = read_toml(&root.join(&m).join("Cargo.toml"))?;
        let name = package_name(&manifest).unwrap_or_else(|| m.clone());
        if name == "rift-store" {
            continue;
        }
        for section in ["dependencies", "build-dependencies"] {
            if let Some(tbl) = manifest.get(section).and_then(Value::as_table) {
                for backend in ["rusqlite", "turso"] {
                    if tbl.contains_key(backend) {
                        violations.push(format!(
                            "{name} depends on `{backend}` (only rift-store may — the Store seam)"
                        ));
                    }
                }
            }
        }
    }
    Ok(violations)
}

/// `[workspace].members` must match the crate/bin directories on disk: every
/// declared member exists, and every `crates/*` and `bins/*` directory with a
/// `Cargo.toml` is declared.
///
/// # Errors
/// If the root manifest cannot be read/parsed.
pub fn check_members_match_dirs(root: &Path) -> Check {
    let declared: BTreeSet<String> = declared_members(root)?.into_iter().collect();
    let mut violations = Vec::new();

    for m in &declared {
        if !root.join(m).join("Cargo.toml").is_file() {
            violations.push(format!("declared member `{m}` has no Cargo.toml on disk"));
        }
    }
    for base in ["crates", "bins"] {
        let dir = root.join(base);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("Cargo.toml").is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let rel = format!("{base}/{name}");
                    if !declared.contains(&rel) {
                        violations.push(format!("crate dir `{rel}` exists but is not a member"));
                    }
                }
            }
        }
    }
    Ok(violations)
}

/// Every member must opt into the workspace lints (`[lints] workspace = true`)
/// and inherit the shared package fields (`edition`/`rust-version`).
///
/// # Errors
/// If a manifest cannot be read/parsed.
pub fn check_member_inheritance(root: &Path) -> Check {
    let mut violations = Vec::new();
    for m in declared_members(root)? {
        let manifest = read_toml(&root.join(&m).join("Cargo.toml"))?;
        let lints_ws = manifest
            .get("lints")
            .and_then(|l| l.get("workspace"))
            .and_then(Value::as_bool)
            == Some(true);
        if !lints_ws {
            violations.push(format!("{m}: missing `[lints] workspace = true`"));
        }
        for field in ["edition", "rust-version"] {
            let inherits = manifest
                .get("package")
                .and_then(|p| p.get(field))
                .and_then(|v| v.get("workspace"))
                .and_then(Value::as_bool)
                == Some(true);
            if !inherits {
                violations.push(format!(
                    "{m}: `package.{field}` does not inherit from workspace"
                ));
            }
        }
    }
    Ok(violations)
}

/// `rust-toolchain.toml` pins an exact numeric channel (not a floating
/// `stable`/`beta`/`nightly` alias) and includes the required components.
///
/// # Errors
/// If the toolchain file cannot be read/parsed.
pub fn check_toolchain_pinned(root: &Path) -> Check {
    let manifest = read_toml(&root.join("rust-toolchain.toml"))?;
    let toolchain = manifest.get("toolchain");
    let mut violations = Vec::new();

    let channel = toolchain
        .and_then(|t| t.get("channel"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Exact numeric pin: starts with a digit, not stable/beta/nightly.
    if !channel.starts_with(|c: char| c.is_ascii_digit()) {
        violations.push(format!(
            "toolchain channel `{channel}` is not an exact numeric pin"
        ));
    }
    let components: BTreeSet<&str> = toolchain
        .and_then(|t| t.get("components"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // `miri` is provisioned by the nightly CI job, not this stable pin.
    for required in ["rustfmt", "clippy", "rust-src"] {
        if !components.contains(required) {
            violations.push(format!("toolchain is missing the `{required}` component"));
        }
    }
    Ok(violations)
}

/// `[workspace.package]` declares the expected MSRV (`1.94`) and edition (`2021`).
///
/// # Errors
/// If the root manifest cannot be read/parsed.
pub fn check_msrv_and_edition(root: &Path) -> Check {
    let manifest = read_toml(&root.join("Cargo.toml"))?;
    let pkg = manifest.get("workspace").and_then(|w| w.get("package"));
    let mut violations = Vec::new();
    let rust_version = pkg
        .and_then(|p| p.get("rust-version"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if rust_version != "1.94" {
        violations.push(format!(
            "workspace rust-version is `{rust_version}`, expected `1.94`"
        ));
    }
    let edition = pkg
        .and_then(|p| p.get("edition"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if edition != "2021" {
        violations.push(format!("workspace edition is `{edition}`, expected `2021`"));
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        workspace_root()
    }

    #[test]
    fn dependency_graph_is_acyclic() {
        assert_eq!(check_acyclic(&root()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn forbids_storage_backend_outside_rift_store() {
        assert_eq!(
            check_storage_backend_isolated(&root()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn workspace_members_match_dirs() {
        assert_eq!(
            check_members_match_dirs(&root()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_member_opts_into_workspace_inheritance() {
        assert_eq!(
            check_member_inheritance(&root()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn toolchain_channel_is_pinned() {
        assert_eq!(
            check_toolchain_pinned(&root()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn workspace_rust_version_is_1_94() {
        assert_eq!(
            check_msrv_and_edition(&root()).unwrap(),
            Vec::<String>::new()
        );
    }

    // --- Unit tests for the check logic itself (fixtures, not the live tree) ---

    #[test]
    fn cycle_detection_flags_a_back_edge() {
        let mut g = BTreeMap::new();
        g.insert("a".to_string(), vec!["b".to_string()]);
        g.insert("b".to_string(), vec!["c".to_string()]);
        g.insert("c".to_string(), vec!["a".to_string()]);
        assert!(find_cycle(&g).is_some());
    }

    #[test]
    fn cycle_detection_passes_a_dag() {
        let mut g = BTreeMap::new();
        g.insert(
            "bin".to_string(),
            vec!["lib".to_string(), "core".to_string()],
        );
        g.insert("lib".to_string(), vec!["core".to_string()]);
        g.insert("core".to_string(), vec![]);
        assert!(find_cycle(&g).is_none());
    }

    #[test]
    fn edges_only_count_in_workspace_and_exclude_dev_deps() {
        let manifest: Value = "
[dependencies]
rift-core = { path = \"../rift-core\" }
serde = \"1\"
[dev-dependencies]
rift-store = { path = \"../rift-store\" }
"
        .parse()
        .unwrap();
        let names: BTreeSet<String> = ["rift-core", "rift-store"]
            .into_iter()
            .map(String::from)
            .collect();
        let edges = workspace_dep_edges(&manifest, &names);
        assert_eq!(edges, vec!["rift-core".to_string()]); // dev-dep excluded, serde external
    }
}
