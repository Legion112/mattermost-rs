//! The parity matrix: assert it tells the truth, and render it.
//!
//! A client with no webview has a ceiling that is not about effort, and
//! `docs/parity.toml` is where that ceiling is written down instead of
//! being rediscovered. A file nobody checks drifts from the code within a
//! release, so three things are asserted here:
//!
//! 1. **A row that claims `done` or `partial` must name endpoints this
//!    client actually builds.** Claiming an endpoint it does not call is
//!    the exact failure the matrix exists to prevent.
//! 2. **A row that claims `wontfix` or `blocked-no-webview` must carry
//!    both a `reason` and a `fallback`.** "We do not do this" without
//!    somewhere to send the reader is a dead end, not a decision.
//! 3. **`verified_against` empty means not yet confirmed against a real
//!    server**, and the count of those is printed every run so it is a
//!    shrinking number rather than a forgotten comment.
//!
//! `docs/parity.md` is generated from the same data, and `--check` fails
//! if the file on disk is not what this would write -- so the readable
//! copy cannot be edited into disagreeing with the source.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Matrix {
    #[serde(default)]
    feature: Vec<Feature>,
}

#[derive(Debug, Deserialize)]
struct Feature {
    id: String,
    area: String,
    status: String,
    #[serde(default)]
    endpoints: Vec<String>,
    #[serde(default)]
    ws_events: Vec<String>,
    #[serde(default)]
    verified_against: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    fallback: String,
    #[serde(default)]
    notes: String,
}

/// The statuses a row may carry. Anything else is a typo, and a typo in
/// `status` would silently skip every assertion below.
const STATUSES: [&str; 5] = [
    "done",
    "partial",
    "planned",
    "wontfix",
    "blocked-no-webview",
];

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let root = repo_root()?;
    let source = root.join("docs/parity.toml");
    let rendered_path = root.join("docs/parity.md");

    let raw = std::fs::read_to_string(&source)
        .map_err(|e| format!("could not read {}: {e}", source.display()))?;
    let matrix: Matrix =
        toml::from_str(&raw).map_err(|e| format!("{} is not valid: {e}", source.display()))?;

    let routes = request_paths(&root)?;
    let mut problems: Vec<String> = Vec::new();
    let mut unverified = 0usize;

    for feature in &matrix.feature {
        if !STATUSES.contains(&feature.status.as_str()) {
            problems.push(format!(
                "{}: status {:?} is not one of {STATUSES:?}",
                feature.id, feature.status
            ));
            continue;
        }
        match feature.status.as_str() {
            "done" | "partial" => {
                for endpoint in &feature.endpoints {
                    let path = normalise_endpoint(endpoint);
                    if !routes.contains(&path) {
                        problems.push(format!(
                            "{}: claims {endpoint:?}, which no request builder in mm-api writes",
                            feature.id
                        ));
                    }
                }
            }
            "wontfix" | "blocked-no-webview" => {
                if feature.reason.trim().is_empty() {
                    problems.push(format!("{}: {} needs a reason", feature.id, feature.status));
                }
                if feature.fallback.trim().is_empty() {
                    problems.push(format!(
                        "{}: {} needs a fallback -- somewhere to send the reader",
                        feature.id, feature.status
                    ));
                }
            }
            _ => {}
        }
        if feature.verified_against.trim().is_empty() {
            unverified += 1;
        }
    }

    if !problems.is_empty() {
        return Err(format!(
            "the parity matrix does not match the code:\n  {}",
            problems.join("\n  ")
        ));
    }

    let rendered = render(&matrix);
    if check {
        let held = std::fs::read_to_string(&rendered_path).unwrap_or_default();
        if held != rendered {
            return Err(format!(
                "{} is not what docs/parity.toml renders to.\n\
                 Run `cargo run -p xtask -- parity` and commit the result.",
                rendered_path.display()
            ));
        }
    } else {
        std::fs::write(&rendered_path, &rendered)
            .map_err(|e| format!("could not write {}: {e}", rendered_path.display()))?;
        println!("parity: wrote {}", rendered_path.display());
    }

    println!(
        "parity: {} features, {unverified} not yet verified against a live server",
        matrix.feature.len()
    );
    Ok(())
}

/// Every path an `mm-api` request builder writes, normalised.
///
/// There is no `ROUTES` table to compare against -- the plan called for
/// one and it was never built -- so the source is read instead. The
/// builders write their paths as `format!("{API}/...")`, which is a
/// narrow enough shape to extract honestly: anything else here would be
/// a route this client cannot issue.
fn request_paths(root: &Path) -> Result<Vec<String>, String> {
    let rest = root.join("crates/mm-api/src/rest.rs");
    let source = std::fs::read_to_string(&rest)
        .map_err(|e| format!("could not read {}: {e}", rest.display()))?;
    let mut out = Vec::new();
    for (at, marker) in source.match_indices("{API}") {
        let tail = &source[at + marker.len()..];
        let Some(end) = tail.find('"') else { continue };
        out.push(normalise_path(&tail[..end]));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `"GET /api/v4/users/{user_id}/teams"` -> `"/users/{}/teams"`.
///
/// The method and the `/api/v4` prefix carry no information the
/// comparison needs, and the placeholder *names* differ on purpose
/// between the matrix (the server's documented names) and the builders
/// (whatever reads best in Rust), so they are flattened.
fn normalise_endpoint(endpoint: &str) -> String {
    let path = endpoint.split_once(' ').map_or(endpoint, |(_, rest)| rest);
    normalise_path(path.trim_start_matches("/api/v4"))
}

fn normalise_path(path: &str) -> String {
    // The query string is not part of the path. A builder that pages or
    // sorts writes one; the matrix documents the endpoint, and nobody
    // writing a row would think to copy `?page={}&per_page={}` into it.
    let path = path.split('?').next().unwrap_or(path);
    let mut out = String::new();
    let mut in_placeholder = false;
    for ch in path.chars() {
        match ch {
            '{' => {
                in_placeholder = true;
                out.push_str("{}");
            }
            '}' => in_placeholder = false,
            _ if in_placeholder => {}
            _ => out.push(ch),
        }
    }
    out.trim_end_matches('/').to_owned()
}

/// Render the matrix as the table a reader wants, grouped by area.
fn render(matrix: &Matrix) -> String {
    let mut by_area: BTreeMap<&str, Vec<&Feature>> = BTreeMap::new();
    for feature in &matrix.feature {
        by_area
            .entry(feature.area.as_str())
            .or_default()
            .push(feature);
    }

    let mut out = String::new();
    out.push_str(
        "# Parity\n\n\
         **Generated from `docs/parity.toml` by `cargo run -p xtask -- parity`.**\n\
         Edit that file, not this one; CI asserts they agree.\n\n\
         | status | meaning |\n\
         |---|---|\n\
         | `done` | implemented and verified against a live server |\n\
         | `partial` | implemented, with the gap named |\n\
         | `planned` | in scope, not built yet |\n\
         | `wontfix` | in scope for upstream, deliberately not for us |\n\
         | `blocked-no-webview` | impossible without a renderer we do not have |\n\n\
         A blank **verified** column means nobody has confirmed it against a\n\
         real server yet.\n",
    );

    for (area, features) in by_area {
        let _ = write!(out, "\n## {area}\n\n");
        out.push_str("| feature | status | verified | notes |\n|---|---|---|---|\n");
        for feature in &features {
            let note = if feature.status == "wontfix" || feature.status == "blocked-no-webview" {
                format!("{} Fallback: `{}`", feature.reason, feature.fallback)
            } else {
                feature.notes.clone()
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} |",
                feature.id,
                feature.status,
                if feature.verified_against.is_empty() {
                    "—"
                } else {
                    &feature.verified_against
                },
                one_line(&note),
            );
        }
        for feature in &features {
            if feature.endpoints.is_empty() && feature.ws_events.is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "\n<details><summary><code>{}</code> wire</summary>\n\n",
                feature.id
            );
            for endpoint in &feature.endpoints {
                let _ = writeln!(out, "- `{endpoint}`");
            }
            for event in &feature.ws_events {
                let _ = writeln!(out, "- ws `{event}`");
            }
            out.push_str("\n</details>\n");
        }
    }
    out
}

/// A table cell is one line, and the notes are paragraphs.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set; run this through cargo".to_owned())?;
    Path::new(&manifest)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{manifest} has no parent"))
}
