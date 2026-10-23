//! The dependency graph of `engine` and `engine-hw` holds none of
//! the crates the dependency replacement removed: only rav1e and symphonia
//! stay as third-party media crates.
//!
//! The graph is read from the workspace `Cargo.lock`, not from `cargo metadata`:
//! metadata wants every locked package on disk, and a lane VPS without the
//! packages that only dev or other-platform builds need cannot run it offline.
//! The lock does not record dependency kinds, so this walk follows build edges
//! as well as normal ones. That is stricter than `cargo tree -e normal`, and it
//! is exact today because none of the three roots declares a dev or build
//! dependency.

use std::collections::{BTreeSet, HashMap, VecDeque};

/// The packages whose dependency graphs are guarded.
const ROOTS: [&str; 2] = ["engine", "engine-hw"];

/// Crates that must not be reachable through a dependency of any root.
const FORBIDDEN: [&str; 9] = [
    "symphonia-codec-aac",
    "mp4",
    "cros-codecs",
    "gbm",
    "image",
    "ravif",
    "flacenc",
    "hound",
    "miniz_oxide",
];

/// Every crate whose name starts with this is forbidden as well.
const FORBIDDEN_PREFIX: &str = "oxideav-";

/// Hardware crates `engine-hw` still linked until the VA-API wave replaced them.
/// Emptied: cros-codecs and gbm are gone; cros-libva is back under ec-va (see ONLY_UNDER).
const PENDING_WAVE2: [&str; 0] = [];

/// Crates that may appear only as a direct dependency of the named crate.
/// cros-libva is the safe VA-API binding under ec-va: it lets ec-va forbid
/// unsafe code, so it stays, but nothing else may pull it in.
const ONLY_UNDER: [(&str, &str); 1] = [("cros-libva", "ec-va")];

/// Crates that may appear only as a direct dependency of `engine`, or as a
/// dependency of one another (symphonia pulls symphonia-core in itself).
const ALLOWED_FROM_ENGINE: [&str; 3] = ["rav1e", "symphonia", "symphonia-core"];

/// One `[[package]]` of Cargo.lock: its key (`name version`), its name, whether
/// it comes from a registry, and its raw dependency entries.
struct Locked {
    key: String,
    name: String,
    registry: bool,
    deps: Vec<String>,
}

/// Reads the `[[package]]` blocks of Cargo.lock. Dependency entries are either
/// `"name"` or `"name version"` (optionally followed by a source), and the
/// version is only written when the name is ambiguous.
fn locked_packages() -> Vec<Locked> {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .expect("read the workspace Cargo.lock");
    let mut packages = Vec::new();
    for block in lock.split("[[package]]\n").skip(1) {
        let mut name = String::new();
        let mut version = String::new();
        let mut registry = false;
        let mut deps = Vec::new();
        let mut in_deps = false;
        for line in block.lines() {
            if in_deps {
                if line == "]" {
                    in_deps = false;
                } else {
                    let entry = line.trim().trim_end_matches(',').trim_matches('"');
                    deps.push(entry.to_string());
                }
            } else if let Some(v) = line.strip_prefix("name = ") {
                name = v.trim_matches('"').to_string();
            } else if let Some(v) = line.strip_prefix("version = ") {
                version = v.trim_matches('"').to_string();
            } else if line.starts_with("source = ") {
                registry = true;
            } else if line == "dependencies = [" {
                in_deps = true;
            }
        }
        packages.push(Locked {
            key: format!("{name} {version}"),
            name,
            registry,
            deps,
        });
    }
    packages
}

/// Turns a lock dependency entry into the key of the package it names.
fn resolve(entry: &str, keys_by_name: &HashMap<&str, Vec<&str>>) -> String {
    let mut parts = entry.split_whitespace();
    let name = parts.next().unwrap();
    match parts.next() {
        Some(version) => format!("{name} {version}"),
        None => {
            let keys = &keys_by_name[name];
            assert_eq!(
                keys.len(),
                1,
                "{name} is ambiguous in Cargo.lock but written bare"
            );
            keys[0].to_string()
        }
    }
}

/// Breadth-first walk over the edges, from `starts`, never entering a package
/// `blocked` returns true for. Maps each reached key to the key it came from.
fn walk(
    starts: &[String],
    edges: &HashMap<&str, Vec<String>>,
    name_of: &HashMap<&str, &str>,
    blocked: impl Fn(&str) -> bool,
) -> HashMap<String, Option<String>> {
    let mut parent: HashMap<String, Option<String>> =
        starts.iter().map(|s| (s.clone(), None)).collect();
    let mut queue: VecDeque<String> = starts.iter().cloned().collect();
    while let Some(key) = queue.pop_front() {
        for next in edges.get(key.as_str()).into_iter().flatten() {
            if !parent.contains_key(next) && !blocked(name_of[next.as_str()]) {
                parent.insert(next.clone(), Some(key.clone()));
                queue.push_back(next.clone());
            }
        }
    }
    parent
}

/// The chain of names from the walk's start to `key`.
fn path_to(
    key: &str,
    parent: &HashMap<String, Option<String>>,
    name_of: &HashMap<&str, &str>,
) -> String {
    let mut names = Vec::new();
    let mut cur = Some(key.to_string());
    while let Some(c) = cur {
        names.push(name_of[c.as_str()].to_string());
        cur = parent[&c].clone();
    }
    names.reverse();
    names.join(" -> ")
}

#[test]
fn no_forbidden_crate_is_in_the_dependency_graph() {
    let packages = locked_packages();
    let mut keys_by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for p in &packages {
        keys_by_name
            .entry(p.name.as_str())
            .or_default()
            .push(p.key.as_str());
    }
    let name_of: HashMap<&str, &str> = packages
        .iter()
        .map(|p| (p.key.as_str(), p.name.as_str()))
        .collect();
    // Workspace members and path packages have no `source`; the roots are among them.
    let roots: Vec<String> = packages
        .iter()
        .filter(|p| !p.registry && ROOTS.contains(&p.name.as_str()))
        .map(|p| p.key.clone())
        .collect();
    assert_eq!(
        roots.len(),
        ROOTS.len(),
        "every guarded root is in Cargo.lock"
    );
    let engine = roots
        .iter()
        .find(|k| name_of[k.as_str()] == "engine")
        .expect("engine is in Cargo.lock")
        .clone();

    let edges: HashMap<&str, Vec<String>> = packages
        .iter()
        .map(|p| {
            let deps = p.deps.iter().map(|d| resolve(d, &keys_by_name)).collect();
            (p.key.as_str(), deps)
        })
        .collect();

    let is_allowed = |name: &str| ALLOWED_FROM_ENGINE.contains(&name);
    let mut violations = Vec::new();

    // Everything reachable from the three roots.
    let parent = walk(&roots, &edges, &name_of, |_| false);
    let mut reached: Vec<&String> = parent.keys().collect();
    reached.sort_unstable();
    let mut seen_pending = BTreeSet::new();
    for key in reached {
        let name = name_of[key.as_str()];
        if PENDING_WAVE2.contains(&name) {
            seen_pending.insert(name);
        } else if FORBIDDEN.contains(&name) || name.starts_with(FORBIDDEN_PREFIX) {
            violations.push(format!(
                "{name} is in the dependency graph: {}",
                path_to(key, &parent, &name_of)
            ));
        }
    }

    // The allowed crates may come in only through engine's own direct dependencies.
    // Walk from everything else, never entering an allowed crate: any allowed crate
    // that walk reaches arrived by another route.
    let mut other_starts: Vec<String> = roots.iter().filter(|r| **r != engine).cloned().collect();
    for dep in edges.get(engine.as_str()).into_iter().flatten() {
        if !is_allowed(name_of[dep.as_str()]) {
            other_starts.push(dep.clone());
        }
    }
    let other = walk(&other_starts, &edges, &name_of, is_allowed);
    let mut stray: Vec<&String> = other.keys().collect();
    stray.sort_unstable();
    for key in stray {
        // A start of this walk is never itself an allowed crate, so this is a route in.
        let name = name_of[key.as_str()];
        if is_allowed(name) {
            violations.push(format!(
                "{name} may only be a direct dependency of engine, but it is reached by: {}",
                path_to(key, &other, &name_of)
            ));
        }
    }
    // Direct allowed dependencies of other roots are caught by the walk above (they are
    // edges out of a start), but a root listing one directly is reported explicitly.
    for root in roots.iter().filter(|r| **r != engine) {
        for dep in edges.get(root.as_str()).into_iter().flatten() {
            if is_allowed(name_of[dep.as_str()]) {
                violations.push(format!(
                    "{} depends on {} directly, but only engine may",
                    name_of[root.as_str()],
                    name_of[dep.as_str()]
                ));
            }
        }
    }

    for (name, owner) in ONLY_UNDER {
        for key in parent.keys().filter(|k| name_of[k.as_str()] == name) {
            for (from, deps) in &edges {
                if parent.contains_key(*from)
                    && name_of[*from] != owner
                    && deps.iter().any(|d| d == key)
                {
                    violations.push(format!(
                        "{name} may only be a dependency of {owner}, but {} depends on it",
                        name_of[*from]
                    ));
                }
            }
        }
    }

    for name in PENDING_WAVE2 {
        if !seen_pending.contains(name) {
            violations.push(format!(
                "{name} is no longer in the dependency graph: delete it from PENDING_WAVE2"
            ));
        }
    }

    violations.sort();
    assert!(
        violations.is_empty(),
        "dependency guard failed:\n  {}",
        violations.join("\n  ")
    );
}
