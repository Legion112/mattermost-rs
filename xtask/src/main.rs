//! Repository automation.
//!
//! **Outside the coverage gate.** Coverage itself is driven from the Makefile
//! (`make cover-gate`) so the exact `cargo llvm-cov` invocation stays visible
//! in one reviewable place rather than being buried behind a second binary.
//! What lives here is the checking that needs real logic.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::ExitCode;

/// Crates inside the 100% line-coverage gate. Pure logic, no I/O.
const GATED: &[&str] = &["mm-api", "mm-core", "mm-md", "mm-config", "mm-desktop"];

/// Crates deliberately outside it, each for a reason stated in its `lib.rs`.
const UNGATED: &[&str] = &[
    "mattermost-rs",
    "mm-types",
    "mm-http",
    "mm-desktop-xdg",
    "mm-gui",
    "mm-headless",
    "mm-lab",
    "xtask",
];

/// Crates a gated crate may never reach, transitively or otherwise.
///
/// This is the invariant the whole gate rests on. A gated crate that can see
/// `tokio` will acquire an `async fn` the first time somebody wants
/// `tokio::time::Duration`, and from then on 100% is unreachable without a
/// live server. Enforcing it mechanically is cheaper than noticing later.
const FORBIDDEN_IN_GATED: &[&str] = &[
    "tokio",
    "reqwest",
    "hyper",
    "tokio-tungstenite",
    "gtk4",
    "glib",
    "libadwaita",
    "relm4",
    "zbus",
    "rusqlite",
    "bollard",
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("check-deps") => run(check_deps()),
        Some(other) => {
            eprintln!("unknown task {other:?}\n{}", usage());
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: cargo xtask <task>\n\n  check-deps   every member is classified, and no gated crate \
     can reach an I/O crate\n"
        .to_owned()
}

fn run(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

struct Metadata {
    /// Workspace member names.
    members: BTreeSet<String>,
    /// Package id -> package name, for every package in the resolve graph.
    names: BTreeMap<String, String>,
    /// Package id -> the ids it depends on.
    edges: BTreeMap<String, Vec<String>>,
    /// Package name -> its id, for workspace members.
    member_ids: BTreeMap<String, String>,
}

fn metadata() -> Result<Metadata, String> {
    let out = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .output()
        .map_err(|e| format!("could not run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("could not parse cargo metadata output: {e}"))?;

    let mut names = BTreeMap::new();
    let mut member_ids = BTreeMap::new();
    let members: BTreeSet<String> = json["workspace_members"]
        .as_array()
        .ok_or("cargo metadata: workspace_members is not an array")?
        .iter()
        .filter_map(|id| id.as_str())
        .filter_map(|id| member_name(id).map(|n| (n.to_owned(), id.to_owned())))
        .map(|(name, id)| {
            member_ids.insert(name.clone(), id);
            name
        })
        .collect();

    for pkg in json["packages"]
        .as_array()
        .ok_or("cargo metadata: packages is not an array")?
    {
        if let (Some(id), Some(name)) = (pkg["id"].as_str(), pkg["name"].as_str()) {
            names.insert(id.to_owned(), name.to_owned());
        }
    }

    let mut edges = BTreeMap::new();
    for node in json["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata: resolve.nodes is not an array")?
    {
        let Some(id) = node["id"].as_str() else {
            continue;
        };
        let deps = node["deps"]
            .as_array()
            .map(|ds| {
                ds.iter()
                    .filter_map(|d| d["pkg"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        edges.insert(id.to_owned(), deps);
    }

    Ok(Metadata {
        members,
        names,
        edges,
        member_ids,
    })
}

/// Pull the crate name out of a workspace-member package id.
///
/// Cargo has used two spellings: the older `name version (source)` and the
/// newer `path+file:///...#name@version` (and `...#version` when the directory
/// is already named after the crate). Handle all three rather than pinning to
/// one, because this check running vacuously is worse than it failing.
fn member_name(id: &str) -> Option<&str> {
    if let Some((_, tail)) = id.split_once('#') {
        return Some(match tail.split_once('@') {
            Some((name, _)) => name,
            // `path+file:///…/crates/mm-core#0.1.0` — the name is the last
            // path segment of the directory.
            None => id
                .split_once('#')?
                .0
                .trim_end_matches('/')
                .rsplit('/')
                .next()?,
        });
    }
    id.split_whitespace().next()
}

fn check_deps() -> Result<(), String> {
    let md = metadata()?;
    let mut problems = Vec::new();

    // 1. Every member is classified. A new crate that is in neither list has
    //    silently either joined or dodged the gate, and nobody decided which.
    let classified: BTreeSet<&str> = GATED.iter().chain(UNGATED).copied().collect();
    for member in &md.members {
        if !classified.contains(member.as_str()) {
            problems.push(format!(
                "workspace member `{member}` is in neither GATED nor UNGATED in \
                 xtask/src/main.rs.\n    Decide which it is: a pure-logic crate goes in GATED and \
                 must reach 100%; anything that touches the network, the GUI or the filesystem \
                 directly goes in UNGATED with the reason stated in its lib.rs."
            ));
        }
    }
    for name in GATED.iter().chain(UNGATED) {
        if !md.members.contains(*name) {
            problems.push(format!(
                "`{name}` is classified in xtask/src/main.rs but is not a workspace member \
                 any more. Remove it from the list."
            ));
        }
    }

    // 2. No gated crate can reach an I/O crate.
    for gated in GATED {
        let Some(root) = md.member_ids.get(*gated) else {
            continue; // already reported above
        };
        for (dep, path) in reachable(&md, root) {
            if FORBIDDEN_IN_GATED.contains(&dep.as_str()) {
                problems.push(format!(
                    "gated crate `{gated}` can reach `{dep}`, which it must not.\n    via {}\n    \
                     A gated crate is pure logic; put the I/O behind a trait and move the \
                     implementation to an ungated crate.",
                    path.join(" -> ")
                ));
            }
        }
    }

    if problems.is_empty() {
        println!(
            "check-deps: {} members classified, {} gated crates free of I/O dependencies",
            md.members.len(),
            GATED.len()
        );
        return Ok(());
    }
    Err(problems
        .iter()
        .map(|p| format!("error: {p}"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Every crate reachable from `root`, with one shortest dependency path to it.
fn reachable(md: &Metadata, root: &str) -> Vec<(String, Vec<String>)> {
    let root_name = md.names.get(root).cloned().unwrap_or_default();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    seen.insert(root);
    let mut queue = VecDeque::new();
    queue.push_back((root.to_owned(), vec![root_name]));
    let mut out = Vec::new();

    while let Some((id, path)) = queue.pop_front() {
        for dep in md.edges.get(&id).map(Vec::as_slice).unwrap_or_default() {
            if !seen.insert(dep.as_str()) {
                continue;
            }
            let name = md.names.get(dep).cloned().unwrap_or_default();
            let mut next = path.clone();
            next.push(name.clone());
            out.push((name, next.clone()));
            queue.push_back((dep.clone(), next));
        }
    }
    out
}
