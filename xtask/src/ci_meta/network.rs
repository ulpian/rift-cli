//! The two halves of "no network in capture": the in-workspace manifest
//! screen and the default-resolved graph screen.
//!
//! They share one definition of what a network client is, which is why they
//! share a module: two lists would be two policies.
use std::path::Path;

/// Every dependency-section KEY a manifest declares, parsed as TOML — so
/// `name = "1"`, `name = { … }` and `[dependencies.name]` are all the same
/// declaration (a mutation round's first bypass was the table form slipping a
/// line matcher). Sections covered: dependencies, dev-dependencies,
/// build-dependencies, and every `[target.'…'.<section>]` variant.
fn declared_dep_keys(manifest: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    for section in sections {
        if let Some(tbl) = manifest.get(section).and_then(toml::Value::as_table) {
            out.extend(tbl.keys().cloned());
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target_tbl in targets.values() {
            for section in sections {
                if let Some(tbl) = target_tbl.get(section).and_then(toml::Value::as_table) {
                    out.extend(tbl.keys().cloned());
                }
            }
        }
    }
    out
}

/// The network clients no shipped binary may resolve. Shared by the manifest
/// screen below and the resolved-graph screen, so the two cannot disagree
/// about what "a network client" is.
const NETWORK_CRATES: [&str; 8] = [
    "ureq",
    "reqwest",
    "hyper",
    "curl",
    "isahc",
    "attohttpc",
    "surf",
    "tokio-tungstenite",
];

/// No crate the `rift` binary links may declare a network client — invariant
/// §1.4 ("no network in capture"), previously asserted only by argument.
///
/// SCOPE, stated exactly (a mutation round rejected the universal claim this
/// docstring used to make): the screen walks the IN-WORKSPACE dependency
/// closure of `rift` (the same `dependency_graph` the adjacency snapshot
/// pins) and checks every reachable member's manifest — all dependency
/// sections, all declaration spellings, TOML-parsed. What it does NOT see:
/// a network client hidden behind an EXTERNAL intermediary crate — that is
/// what [`no_network_crate_in_the_resolved_graph`] sees, over the
/// default-resolved `cargo tree` of both binaries. Both of that round's
/// bypasses — the `[dependencies.ureq]` table form, and a plain dep
/// in `rift-config`, which the hook links but a hardcoded list omitted —
/// are covered by construction and pinned in the calibration test.
///
/// # Errors
/// If the workspace graph or a member manifest cannot be read.
pub fn no_network_deps_in_capture_crates(root: &Path) -> Result<Vec<String>, String> {
    let graph = crate::dependency_graph(root)?;
    // BFS the in-workspace closure of the binary that hosts the hooks.
    let mut reach = std::collections::BTreeSet::new();
    let mut queue = vec!["rift".to_string()];
    while let Some(n) = queue.pop() {
        if reach.insert(n.clone()) {
            if let Some(edges) = graph.get(&n) {
                queue.extend(edges.iter().cloned());
            }
        }
    }
    let members = crate::declared_members(root)?;
    let mut v = Vec::new();
    for m in &members {
        let rel = format!("{m}/Cargo.toml");
        let manifest = crate::read_toml(&root.join(&rel))?;
        let Some(name) = crate::package_name(&manifest) else {
            continue;
        };
        if !reach.contains(&name) {
            continue;
        }
        for key in declared_dep_keys(&manifest) {
            if NETWORK_CRATES.contains(&key.as_str()) {
                v.push(format!(
                    "{rel}: crate `{name}` is in the rift binary's closure and declares \
                     network client `{key}` — invariant §1.4 (no network in capture)"
                ));
            }
        }
    }
    Ok(v)
}

/// The network crates named in a `cargo tree --prefix none --format {p}`
/// listing (first word of each line is the package name). An EMPTY listing
/// is an error, not a pass: a screen that reads nothing must not report
/// "nothing found".
fn network_crates_in_tree(listing: &str) -> Result<Vec<String>, String> {
    let names: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    if names.is_empty() {
        return Err("cargo tree returned no packages — the resolved-graph screen read nothing and fails closed".to_owned());
    }
    let mut found: Vec<String> = names
        .iter()
        .filter(|n| NETWORK_CRATES.contains(n))
        .map(|n| (*n).to_owned())
        .collect();
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The DEFAULT-resolved normal-dependency graph of the two shipped binaries
/// (`rift`, `rift-memd`) names no `NETWORK_CRATES` member — the external
/// half of invariant §1.4 that the manifest screen cannot see, and the
/// executable form of the same rule. No workspace crate depends on an HTTP
/// client today, and this is what reds if one is added behind an external
/// intermediary the manifest screen cannot read.
///
/// # Errors
/// If `cargo tree` cannot run, fails, or returns no packages (fail closed).
pub fn no_network_crate_in_the_resolved_graph(root: &Path) -> Result<Vec<String>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new(cargo)
        .current_dir(root)
        .args([
            "tree",
            "-p",
            "rift",
            "-p",
            "rift-memd",
            "-e",
            "normal",
            "--locked",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .map_err(|e| format!("cargo tree (resolved-graph screen): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree (resolved-graph screen) failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(network_crates_in_tree(&listing)?
        .into_iter()
        .map(|c| {
            format!(
                "resolved graph: `{c}` is reachable from `rift` or `rift-memd` in the \
                 default build — invariant §1.4 (no network in capture); a network \
                 client belongs behind an opt-in feature only"
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
    use crate::workspace_root;

    /// The mutation round's two bypasses, pinned: the TOML table form is the
    /// same declaration as the inline form once parsed, and the screen's
    /// crate SET is the binary's graph closure, not a hardcoded list.
    #[test]
    fn the_network_screen_sees_every_declaration_spelling() {
        let inline: toml::Value = toml::from_str("[dependencies]\nureq = \"3\"").unwrap();
        let table: toml::Value = toml::from_str("[dependencies.ureq]\nversion = \"3\"").unwrap();
        let dev: toml::Value = toml::from_str("[dev-dependencies]\nreqwest = \"0.12\"").unwrap();
        let target: toml::Value =
            toml::from_str("[target.'cfg(unix)'.dependencies.curl]\nversion = \"0.4\"").unwrap();
        for v in [&inline, &table, &dev, &target] {
            assert!(
                !declared_dep_keys(v).is_empty(),
                "every spelling is a declaration"
            );
        }
        assert!(declared_dep_keys(&table).contains(&"ureq".to_string()));
        let mention: toml::Value = toml::from_str("[package]\nname = \"x\"").unwrap();
        assert!(declared_dep_keys(&mention).is_empty());
    }

    /// Bypass 2's shape: the screened set must include every workspace crate
    /// the rift binary reaches — rift-config was the planted escape, linked by
    /// the hook on every commit yet absent from the old hardcoded list.
    /// rift-shadow-redact is planted too: its own manifest test
    /// (`heavy_model_pass_has_no_dependency_to_stand_on`) is an 11-name
    /// denylist over one file, and the structural claim that crate used to
    /// make for itself is held HERE, by closure membership.
    #[test]
    fn the_screened_set_is_the_binary_closure_and_covers_the_planted_escape() {
        let root = workspace_root();
        let graph = crate::dependency_graph(&root).unwrap();
        let mut reach = std::collections::BTreeSet::new();
        let mut queue = vec!["rift".to_string()];
        while let Some(n) = queue.pop() {
            if reach.insert(n.clone()) {
                if let Some(edges) = graph.get(&n) {
                    queue.extend(edges.iter().cloned());
                }
            }
        }
        for planted in [
            "rift-config",
            "rift-git",
            "rift-shadow",
            "rift-store",
            "rift-shadow-redact",
        ] {
            assert!(reach.contains(planted), "{planted} must be screened");
        }
    }

    /// The resolved-graph screen names a network crate by its first word and
    /// fails closed on an empty listing rather than passing it.
    #[test]
    fn a_resolved_graph_naming_a_network_crate_is_refused_and_an_empty_one_fails_closed() {
        let clean =
            "rift v0.0.1 (/w/bins/rift)\nserde v1.0.0\nrift-memd v0.0.1 (/w/bins/rift-memd)\n";
        assert!(network_crates_in_tree(clean).unwrap().is_empty());
        let dirty = format!("{clean}ureq v3.0.0\nhyper v1.0.0\nureq v3.0.0\n");
        assert_eq!(
            network_crates_in_tree(&dirty).unwrap(),
            vec!["hyper", "ureq"]
        );
        assert!(
            network_crates_in_tree("").is_err(),
            "empty output must fail closed"
        );
        assert!(network_crates_in_tree("\n\n").is_err());
        assert!(no_network_crate_in_the_resolved_graph(&workspace_root())
            .unwrap()
            .is_empty());
    }

    /// The manifest screen itself over a planted workspace: a member the `rift`
    /// binary reaches that declares a network client in the table spelling is
    /// reported, and a member outside that closure declaring one is not.
    #[test]
    fn a_network_client_in_the_binary_closure_is_reported_and_one_outside_it_is_not() {
        let root = scratch_dir("network-deps");
        let manifest = |rel: &str, body: &str| {
            std::fs::create_dir_all(root.join(rel)).unwrap();
            std::fs::write(root.join(rel).join("Cargo.toml"), body).unwrap();
        };
        manifest(
            "",
            "[workspace]\nmembers = [\"bins/rift\", \"crates/leaf\", \"crates/outside\"]\n",
        );
        manifest(
            "bins/rift",
            "[package]\nname = \"rift\"\n\n[dependencies]\nleaf = { path = \"../../crates/leaf\" }\n",
        );
        manifest(
            "crates/leaf",
            "[package]\nname = \"leaf\"\n\n[dependencies.ureq]\nversion = \"3\"\n",
        );
        manifest(
            "crates/outside",
            "[package]\nname = \"outside\"\n\n[dependencies]\nreqwest = \"0.12\"\n",
        );

        let violations = no_network_deps_in_capture_crates(&root).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations.first().unwrap().starts_with(
                "crates/leaf/Cargo.toml: crate `leaf` is in the rift binary's closure and \
                 declares network client `ureq`"
            ),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
