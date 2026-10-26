//! `cargo xtask coverage` — the 100% line-coverage gate.
//!
//! This computes the verdict from `cargo llvm-cov`'s **JSON export** rather
//! than from its summary table, because the summary table is wrong. Two
//! separate accounting bugs in cargo-llvm-cov 0.9.1 forced this:
//!
//! 1. `--fail-under-file-lines 100` exits 1 even when every file in the report
//!    is at 100.00% lines, and prints nothing when it does.
//! 2. The summary table reported `mm-api/src/http.rs` as having 1 missed line
//!    while the JSON export *and* the generated per-line HTML both showed all
//!    225 rows covered. The table and the data disagreed.
//!
//! The JSON is the same data the HTML is rendered from, so trusting it is not
//! a workaround — it is reading the measurement instead of a derived summary.
//! It also lets this gate do what `--fail-under-lines` cannot: name the exact
//! uncovered lines, with the source text, so a red gate is actionable from the
//! CI log alone.
//!
//! Test code is excluded. Every gated crate keeps its unit tests in
//! `src/tests/<module>.rs`, wired up with `#[cfg(test)] #[path = ...] mod
//! tests;`, so they are still child modules with access to private items but
//! live under a path the ignore regex matches. That is deliberate: coverage
//! should answer "is the production code exercised?", and a test's own
//! unexecuted `panic!("unexpected: …")` arm is not a gap in production code.
//! Before this split, 29 of 30 "uncovered" lines were assertion messages and
//! unreachable match arms inside test modules.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

/// Files whose coverage is measured but not required to be complete. Kept
/// empty on purpose; an entry here is a debt, not a convenience.
const IGNORE_REGEX: &str = r"(^|/)(tests|benches|examples)/|unreachable\.rs|/fakes/";

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let json_path = std::env::temp_dir().join("mmrs-coverage.json");

    // A stale .profraw set from a differently-configured run both inflates and
    // corrupts the merge, so always start clean.
    run_cargo(&["llvm-cov", "clean", "--workspace"])?;

    let mut cargo_args: Vec<String> = vec![
        "llvm-cov".into(),
        "--locked".into(),
        "--all-features".into(),
        "--ignore-filename-regex".into(),
        IGNORE_REGEX.into(),
        "--json".into(),
        "--output-path".into(),
        json_path.display().to_string(),
    ];
    for pkg in crate::deps::GATED {
        cargo_args.push("-p".into());
        cargo_args.push((*pkg).to_string());
    }
    let borrowed: Vec<&str> = cargo_args.iter().map(String::as_str).collect();
    run_cargo(&borrowed)?;

    let raw = std::fs::read(&json_path)
        .map_err(|e| format!("could not read {}: {e}", json_path.display()))?;
    let report: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| format!("could not parse the coverage JSON: {e}"))?;

    let uncovered = uncovered_lines(&report)?;
    let measured = measured_lines(&report)?;

    if uncovered.is_empty() {
        println!(
            "coverage: {measured} production lines across {} gated crates, all covered",
            crate::deps::GATED.len()
        );
        return Ok(());
    }

    if args.iter().any(|a| a == "--report-only") {
        for (file, lines) in &uncovered {
            println!("{file}: {}", join(lines));
        }
        return Ok(());
    }

    let mut msg = format!(
        "coverage: {} uncovered line(s) in the gated crates, which must be at 100%:\n",
        uncovered.values().map(Vec::len).sum::<usize>()
    );
    for (file, lines) in &uncovered {
        for line in lines {
            let text = source_line(file, *line).unwrap_or_default();
            writeln!(msg, "  {file}:{line}  {}", text.trim())
                .map_err(|e| format!("could not build the coverage report: {e}"))?;
        }
    }
    msg.push_str(
        "\nEither exercise these lines, or ask whether they belong in a gated crate at all: \
         an unreachable branch is usually a sign the state should have been made \
         unrepresentable. See the plan's note on what fights a 100% gate.",
    );
    Err(msg)
}

/// Lines with at least one executable segment, none of which ran.
///
/// A line can carry several segments (a closure, a `?`, a short-circuit), so
/// the rule is: take the highest execution count on the line and treat the
/// line as uncovered only when that is zero. This is the same rule the HTML
/// renderer uses, which is why the two agree where the summary table does not.
fn uncovered_lines(report: &serde_json::Value) -> Result<BTreeMap<String, Vec<u64>>, String> {
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for file in files(report)? {
        let name = short_name(file);
        let mut best: BTreeMap<u64, u64> = BTreeMap::new();
        for seg in file
            .get("segments")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let Some(parts) = seg.as_array() else {
                continue;
            };
            // [line, col, count, has_count, is_region_entry, is_gap_region]
            let line = parts.first().and_then(serde_json::Value::as_u64);
            let count = parts.get(2).and_then(serde_json::Value::as_u64);
            let has_count = parts.get(3).and_then(serde_json::Value::as_bool);
            let is_gap = parts
                .get(5)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if let (Some(line), Some(count), Some(true)) = (line, count, has_count) {
                if is_gap {
                    continue;
                }
                let slot = best.entry(line).or_insert(0);
                *slot = (*slot).max(count);
            }
        }
        let zero: Vec<u64> = best
            .into_iter()
            .filter(|(_, count)| *count == 0)
            .map(|(line, _)| line)
            .collect();
        if !zero.is_empty() {
            out.insert(name, zero);
        }
    }
    Ok(out)
}

fn measured_lines(report: &serde_json::Value) -> Result<u64, String> {
    let mut total = 0;
    for file in files(report)? {
        total += file
            .pointer("/summary/lines/count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
    }
    Ok(total)
}

fn files(report: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    report
        .pointer("/data/0/files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the coverage JSON has no data[0].files array".to_owned())
}

fn short_name(file: &serde_json::Value) -> String {
    let full = file
        .get("filename")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<unknown>");
    full.rsplit_once("mattermost-rs/")
        .map_or(full, |(_, tail)| tail)
        .to_owned()
}

fn source_line(file: &str, line: u64) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let idx = usize::try_from(line).ok()?.checked_sub(1)?;
    text.lines().nth(idx).map(str::to_owned)
}

fn join(lines: &[u64]) -> String {
    lines
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn run_cargo(args: &[&str]) -> Result<(), String> {
    let status = Command::new(env!("CARGO"))
        .args(args)
        .status()
        .map_err(|e| format!("could not run cargo {}: {e}", args.join(" ")))?;
    if status.success() {
        return Ok(());
    }
    if args.contains(&"clean") {
        // A failed clean is not worth stopping for; the measurement that
        // follows is what matters.
        return Ok(());
    }
    Err(format!(
        "cargo {} failed with {status}. If the tool is missing: \
         cargo install cargo-llvm-cov",
        args.join(" ")
    ))
}
