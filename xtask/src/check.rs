//! Run the gate's checks in parallel, and say plainly what failed.
//!
//! The checks split into two streams by what they *build*, not by what
//! they assert. `clippy` and `cargo test` share `target/` and take turns
//! on its lock; `cover-gate` builds everything again with instrumenting
//! rustflags into `target/llvm-cov-target`, so it has a lock and a cache
//! of its own and can run alongside them. Those two streams together are
//! most of the wall clock back. A third stream would only queue behind
//! one of the existing locks.
//!
//! This is Rust rather than `make -j` because of what happens when a
//! check fails. With `make -O` the output of the losing target is
//! buffered, and make tears the other jobs down before flushing it: the
//! first parallel run of this gate reported `Error 2` and nothing else,
//! which is worse than a slow gate. Here each step's output is captured
//! whole and printed when the run ends, so a failure is always legible
//! and always attributed.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Step {
    name: &'static str,
    args: &'static [&'static str],
}

/// Everything that shares `target/`, in the order that fails soonest:
/// formatting before compiling, compiling before running.
const SHARED: &[Step] = &[
    Step {
        name: "fmt",
        args: &["fmt", "--all", "--check"],
    },
    Step {
        name: "clippy",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    },
    Step {
        name: "test",
        args: &["test", "--workspace", "--all-features", "--locked"],
    },
    Step {
        name: "check-deps",
        args: &["run", "-q", "-p", "xtask", "--", "check-deps"],
    },
    Step {
        name: "parity",
        args: &["run", "-q", "-p", "xtask", "--", "parity", "--check"],
    },
];

/// The instrumented build, which shares nothing with the above.
const INSTRUMENTED: &[Step] = &[Step {
    name: "cover-gate",
    args: &["run", "-q", "-p", "xtask", "--", "coverage"],
}];

struct Outcome {
    name: &'static str,
    ok: bool,
    took: Duration,
    output: Vec<u8>,
}

pub(crate) fn run(_args: &[String]) -> Result<(), String> {
    let started = Instant::now();
    let (shared, instrumented) = std::thread::scope(|scope| {
        let a = scope.spawn(|| stream(SHARED));
        let b = scope.spawn(|| stream(INSTRUMENTED));
        (a.join(), b.join())
    });
    let mut done = shared.map_err(|_| "the shared-target stream panicked".to_owned())?;
    done.extend(instrumented.map_err(|_| "the instrumented stream panicked".to_owned())?);

    for outcome in &done {
        println!(
            "{:>10}  {:>6.1}s  {}",
            outcome.name,
            outcome.took.as_secs_f32(),
            if outcome.ok { "ok" } else { "FAILED" }
        );
    }

    let failed: Vec<&Outcome> = done.iter().filter(|o| !o.ok).collect();
    if failed.is_empty() {
        println!(
            "check: all passed in {:.1}s",
            started.elapsed().as_secs_f32()
        );
        return Ok(());
    }
    // The whole of what each failure said. A gate that reports only that
    // something went wrong is a gate somebody re-runs serially to find
    // out what, which is the time this was meant to save.
    for outcome in &failed {
        println!("\n----- {} -----", outcome.name);
        let _ = std::io::stdout().write_all(&outcome.output);
    }
    Err(format!(
        "check: {} failed",
        failed.iter().map(|o| o.name).collect::<Vec<_>>().join(", ")
    ))
}

/// Run one stream in order, stopping at the first failure: everything
/// after it would be compiling against code that does not pass.
fn stream(steps: &'static [Step]) -> Vec<Outcome> {
    let mut out = Vec::new();
    for step in steps {
        let at = Instant::now();
        let result = Command::new(cargo())
            .args(step.args)
            .stdin(Stdio::null())
            .output();
        let (ok, bytes) = match result {
            Ok(done) => {
                let mut bytes = done.stdout;
                bytes.extend_from_slice(&done.stderr);
                (done.status.success(), bytes)
            }
            Err(e) => (false, format!("could not run cargo: {e}").into_bytes()),
        };
        let failed = !ok;
        out.push(Outcome {
            name: step.name,
            ok,
            took: at.elapsed(),
            output: bytes,
        });
        if failed {
            break;
        }
    }
    out
}

/// The cargo that invoked this task, so a toolchain override holds.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}
