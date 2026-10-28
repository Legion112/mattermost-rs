//! Frame and bind timings, behind `MMRS_PERF`.
//!
//! Scrolling is the one thing in this window that cannot be argued about
//! from the source: whether it is smooth is a distribution of frame
//! intervals, and the only honest way to compare two versions of the code
//! is to measure both the same way.
//!
//! So this drives the scroll itself rather than waiting for a hand on a
//! wheel. A fixed number of pixels per frame, from the top of the channel
//! to the bottom, recording how long each frame took and how much of that
//! went into binding rows. Reproducible, scriptable, and it needs no
//! pointer -- which means it runs in the test container like everything
//! else.
//!
//! Compiled in always and asleep unless the variable is set: a perf mode
//! that has to be enabled at build time is one that is never run.
//!
//! Everything here prints to **stderr**. Not taste: `println!` to a pipe
//! is block-buffered, and the first run of this inside a container
//! produced no output at all because the process was stopped before the
//! buffer was flushed.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;

/// How far to move per frame. Large enough to cross a long channel in a
/// few seconds, small enough that every row on the way is realized and
/// bound rather than skipped over.
const STEP: f64 = 120.0;

/// A frame budget at 60Hz. Anything over this is a frame the eye can see.
const BUDGET: Duration = Duration::from_micros(16_667);

/// How many frames to measure. At 60Hz this is about fifteen seconds,
/// which is long enough to cross several pages of history.
const FRAMES: usize = 900;

/// How long to sit at the top waiting for older messages before deciding
/// there are none. Generous: the request is a round trip and a splice.
const PATIENCE_FRAMES: u32 = 180;

thread_local! {
    static STATE: RefCell<Option<Run>> = const { RefCell::new(None) };
    /// Which sweep attachment is the live one. A list is shown, then
    /// another, and only the last one before the sweep starts is the
    /// one with the channel in it.
    static CLAIMED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Default)]
struct Run {
    frames: Vec<Duration>,
    binds: Vec<Duration>,
    /// Anything else worth timing, by name: how many times and how long
    /// in total. Named rather than a field each, so adding a probe is one
    /// line at the place being measured.
    marks: std::collections::BTreeMap<&'static str, (u32, Duration)>,
    /// The marks as they stood at the last tick, so a slow frame can
    /// be charged to what moved during it.
    seen: std::collections::BTreeMap<&'static str, (u32, Duration)>,
    last: Option<i64>,
}

/// A frame worth explaining. Well over budget, because explaining every
/// 17ms frame would drown the report in the ordinary.
const ATTRIBUTE_OVER: Duration = Duration::from_millis(60);

/// Whether to measure at all.
pub(crate) fn watching() -> bool {
    std::env::var_os("MMRS_PERF").is_some()
}

/// `MMRS_PERF=startup` measures the main loop from launch without
/// touching the scroll position.
///
/// A channel that will not open for seven seconds is not waiting on the
/// network -- the window draws what it already holds before it asks for
/// anything -- so what is being measured is whether the GUI thread is
/// free to notice the click at all.
pub(crate) fn watching_startup() -> bool {
    std::env::var("MMRS_PERF").is_ok_and(|v| v == "startup")
}

/// How long to wait before sweeping, so the channel under test can be
/// opened first.
pub(crate) fn delay() -> Duration {
    let seconds = std::env::var("MMRS_PERF_AFTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    Duration::from_secs(seconds)
}

/// Record the cost of binding one row.
pub(crate) fn bound(took: Duration) {
    if !watching() {
        return;
    }
    STATE.with_borrow_mut(|state| {
        if let Some(run) = state.as_mut() {
            run.binds.push(took);
        }
    });
}

/// Scroll back through the open channel, a step per frame, and say what
/// it cost.
///
/// Upwards from the bottom, not downwards from the top, because that is
/// what reading back through a channel is and because it is the direction
/// that pages: reaching the top asks the server for older messages and
/// splices them in above, and whatever that costs belongs in the
/// measurement rather than outside it.
///
/// Stops after a fixed number of frames rather than at the top, because
/// a channel with a year of history has no top worth reaching.
pub(crate) fn sweep(scroller: &gtk::ScrolledWindow) {
    // Once a sweep has begun it owns the window; before then, the list
    // to measure is whichever one is on show. There is a list per
    // channel now, so the one that existed when the window opened is
    // usually the empty placeholder -- which measured 180 perfect
    // frames of nothing at all.
    if STATE.with_borrow(Option::is_some) {
        return;
    }
    let mine = CLAIMED.with(|c| {
        let next = c.get() + 1;
        c.set(next);
        next
    });
    if watching_switch() {
        // Nothing to sweep: this mode measures clicks, not scrolling.
        return;
    }
    if watching_startup() {
        watch_startup(scroller);
        return;
    }
    eprintln!("perf: sweeping once {:?} of frames have passed", delay());

    // Everything hangs off the frame clock, including the wait before the
    // sweep starts. A `glib::timeout` was the obvious way to wait and it
    // never fired in this window -- measured, not assumed -- whereas tick
    // callbacks are what drive the drawing being measured and are
    // therefore the thing that cannot be quietly absent.
    let began = std::cell::Cell::new(0i64);
    let waited = std::cell::Cell::new(0u32);
    scroller.add_tick_callback(move |widget, clock| {
        // A later list took the sweep over: this one is a page nobody
        // is looking at.
        if CLAIMED.with(std::cell::Cell::get) != mine {
            return glib::ControlFlow::Break;
        }
        let now = clock.frame_time();
        if began.get() == 0 {
            began.set(now);
            return glib::ControlFlow::Continue;
        }
        let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
            return glib::ControlFlow::Break;
        };
        let adjustment = scroller.vadjustment();

        let elapsed = Duration::from_micros(now.saturating_sub(began.get()).unsigned_abs());
        if elapsed < delay() {
            return glib::ControlFlow::Continue;
        }

        let enough = STATE.with_borrow_mut(|state| {
            let run = state.get_or_insert_with(|| {
                eprintln!("perf: sweeping back from {:.0}px", adjustment.value());
                Run::default()
            });
            if let Some(before) = run.last {
                let frame = Duration::from_micros(now.saturating_sub(before).unsigned_abs());
                run.frames.push(frame);
                // What a slow frame had in it, named from the marks that
                // moved since the last tick. A percentile says how bad;
                // this says what, which is the question a fix needs.
                if frame > ATTRIBUTE_OVER {
                    let moved: Vec<String> = run
                        .marks
                        .iter()
                        .filter_map(|(what, (times, took))| {
                            let (seen_times, seen_took) =
                                run.seen.get(what).copied().unwrap_or_default();
                            let more = took.saturating_sub(seen_took);
                            (times > &seen_times && more > Duration::from_millis(1)).then(|| {
                                format!(
                                    "{what} {}x {:.0}ms",
                                    times - seen_times,
                                    more.as_secs_f64() * 1000.0
                                )
                            })
                        })
                        .collect();
                    eprintln!(
                        "  frame {} took {:.0}ms: {}",
                        run.frames.len(),
                        frame.as_secs_f64() * 1000.0,
                        if moved.is_empty() {
                            "nothing marked (layout, draw, or something unmeasured)".to_owned()
                        } else {
                            moved.join(", ")
                        }
                    );
                }
                run.seen.clone_from(&run.marks);
            }
            run.last = Some(now);
            run.frames.len() >= FRAMES
        });
        if enough {
            report("scroll");
            return glib::ControlFlow::Break;
        }

        let next = adjustment.value() - STEP;
        if next <= 0.0 {
            adjustment.set_value(0.0);
            // Resting at the top is what makes the client ask for the page
            // before this one. Give it a moment; if nothing ever arrives,
            // the channel has no more history to measure.
            waited.set(waited.get() + 1);
            if waited.get() > PATIENCE_FRAMES {
                report("scroll");
                return glib::ControlFlow::Break;
            }
            return glib::ControlFlow::Continue;
        }
        waited.set(0);
        adjustment.set_value(next);
        glib::ControlFlow::Continue
    });
}

/// Record the main loop from launch, scrolling nothing.
///
/// Every frame interval while the window starts up, syncs, warms its
/// channels and draws its sidebar. A frame that takes a second is a
/// second in which no click is noticed, and that is what a channel
/// taking seven seconds to open actually is.
fn watch_startup(scroller: &gtk::ScrolledWindow) {
    eprintln!(
        "perf: watching the main loop from launch -- switch between channels \
         while this runs, and it will report after {FRAMES} frames"
    );
    STATE.with_borrow_mut(|state| *state = Some(Run::default()));
    scroller.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        let enough = STATE.with_borrow_mut(|state| {
            let Some(run) = state.as_mut() else {
                return true;
            };
            if let Some(before) = run.last {
                run.frames.push(Duration::from_micros(
                    now.saturating_sub(before).unsigned_abs(),
                ));
            }
            run.last = Some(now);
            run.frames.len() >= FRAMES
        });
        if enough {
            report("startup");
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// Print what was measured, and forget it.
fn report(what: &str) {
    STATE.with_borrow_mut(|state| {
        let Some(run) = state.take() else {
            return;
        };
        let frames_in_order = run.frames.clone();
        let mut frames = run.frames;
        if frames.is_empty() {
            eprintln!("perf {what}: no frames recorded");
            return;
        }
        frames.sort_unstable();
        let over = frames.iter().filter(|f| **f > BUDGET).count();
        let total: Duration = frames.iter().sum();
        let binds: Duration = run.binds.iter().sum();
        eprintln!(
            "perf {what}: {} frames over {:.1}s\n  \
             p50 {:>6.1}ms  p95 {:>6.1}ms  p99 {:>6.1}ms  max {:>6.1}ms\n  \
             {over} frame(s) over 16.7ms ({:.1}%)\n  \
             {} row binds costing {:.0}ms total, {:.2}ms each",
            frames.len(),
            total.as_secs_f64(),
            millis(percentile(&frames, 50)),
            millis(percentile(&frames, 95)),
            millis(percentile(&frames, 99)),
            millis(*frames.last().unwrap_or(&Duration::ZERO)),
            100.0 * count(over) / count(frames.len()),
            run.binds.len(),
            binds.as_secs_f64() * 1000.0,
            if run.binds.is_empty() {
                0.0
            } else {
                binds.as_secs_f64() * 1000.0 / count(run.binds.len())
            },
        );
        // Early frames against late ones. A window whose cost grows with
        // the number of rows it is holding shows up here and nowhere else:
        // the percentiles above average the two together and hide it.
        let half = frames_in_order.len() / 2;
        let (first, second) = frames_in_order.split_at(half);
        let mean = |fs: &[Duration]| -> f64 {
            if fs.is_empty() {
                return 0.0;
            }
            fs.iter().sum::<Duration>().as_secs_f64() * 1000.0 / count(fs.len())
        };
        eprintln!(
            "  first half {:>6.1}ms mean, second half {:>6.1}ms mean",
            mean(first),
            mean(second)
        );
        report_marks(&run.marks);
    });
}

fn report_marks(marks: &std::collections::BTreeMap<&'static str, (u32, Duration)>) {
    for (what, (times, took)) in marks {
        eprintln!(
            "  {what:<22} {times:>5} x  {:>7.0}ms total  {:>6.2}ms each",
            took.as_secs_f64() * 1000.0,
            took.as_secs_f64() * 1000.0 / f64::from(*times),
        );
    }
}

fn percentile(sorted: &[Duration], which: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let at = (sorted.len() * which / 100).min(sorted.len() - 1);
    sorted[at]
}

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A count as a divisor. `usize` does not convert to `f64` without losing
/// precision above 2^53, and a frame count never comes near that; going
/// through `u32` says so to the compiler rather than silencing the lint.
fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Time one closure, when measuring.
pub(crate) fn timed<T>(f: impl FnOnce() -> T) -> T {
    if !watching() {
        return f();
    }
    let started = Instant::now();
    let out = f();
    bound(started.elapsed());
    out
}

/// File a duration under a name, when the work is not a closure.
pub(crate) fn record(what: &'static str, took: Duration) {
    if !watching() {
        return;
    }
    STATE.with_borrow_mut(|state| {
        if let Some(run) = state.as_mut() {
            let entry = run.marks.entry(what).or_insert((0, Duration::ZERO));
            entry.0 += 1;
            entry.1 += took;
        }
    });
}

/// Time one closure and file it under a name.
///
/// For the costs that are not per-row: applying a page to the store,
/// rebuilding the model, and anything else a stall might turn out to be.
pub(crate) fn marked<T>(what: &'static str, f: impl FnOnce() -> T) -> T {
    if !watching() {
        return f();
    }
    let started = Instant::now();
    let out = f();
    let took = started.elapsed();
    STATE.with_borrow_mut(|state| {
        if let Some(run) = state.as_mut() {
            let entry = run.marks.entry(what).or_insert((0, Duration::ZERO));
            entry.0 += 1;
            entry.1 += took;
        }
    });
    out
}

/// `MMRS_PERF=switch` traces channel switches rather than frames.
///
/// A channel that takes five seconds to appear is one of two things,
/// and a screencast cannot tell them apart: a main loop too busy to
/// draw, or a window that had nothing of its own to draw and sat
/// waiting for the server. The fixes are opposite, so the trace says
/// which -- how many rows the click drew out of local state, and how
/// long after it the answer from the network changed them.
pub(crate) fn watching_switch() -> bool {
    std::env::var("MMRS_PERF").is_ok_and(|v| v == "switch")
}

thread_local! {
    /// The channel last clicked, when it was clicked, and how many
    /// changes to it have been reported since.
    static SWITCH: RefCell<Option<Switch>> = const { RefCell::new(None) };
}

/// The channel on screen, and what has happened to it since the click
/// that opened it.
struct Switch {
    channel: String,
    /// What the sidebar calls it, learned on the first redraw.
    name: String,
    began: Instant,
    drawn: u32,
}

/// How many later changes to a channel are worth printing before the
/// reader has the picture. The first is the one that matters; a couple
/// more show whether it settled or kept moving.
const CHANGES_SHOWN: u32 = 3;

/// How long after a click a channel has had its say. Past this it is
/// answering the background rather than the reader.
const SETTLED: Duration = Duration::from_secs(6);

/// A channel was clicked.
pub(crate) fn switch_began(channel: &str) {
    if !watching_switch() {
        return;
    }
    SWITCH.with_borrow_mut(|switch| {
        // What the channel just left behind: every redraw it took while
        // it was open. A channel that is still being redrawn long after
        // it settled is one the background is dragging around, and that
        // is what leaves no main loop for the next click.
        if let Some(left) = switch.as_ref() {
            eprintln!(
                "  ({} was redrawn {} times over {:.1}s)",
                left.name,
                left.drawn,
                left.began.elapsed().as_secs_f64()
            );
            // And where that time went. The marks are the same ones the
            // scroll sweep reports; here they are emptied between
            // clicks, so each block belongs to one channel's visit
            // rather than to the whole run.
            STATE.with_borrow_mut(|state| {
                if let Some(run) = state.as_mut() {
                    report_marks(&run.marks);
                    run.marks.clear();
                }
            });
        }
        // The marks have nowhere to go until a run exists. The sweep
        // makes one when it starts; this mode never sweeps.
        STATE.with_borrow_mut(|state| {
            if state.is_none() {
                *state = Some(Run::default());
            }
        });
        *switch = Some(Switch {
            channel: channel.to_owned(),
            name: channel.to_owned(),
            began: Instant::now(),
            drawn: 0,
        });
    });
    // And again once it has had time to settle, so that a channel
    // somebody opens and then reads -- which is most of them -- says
    // where its time went without waiting for the next click.
    glib::timeout_add_local_once(SETTLED, || {
        SWITCH.with_borrow(|switch| {
            let Some(open) = switch.as_ref() else {
                return;
            };
            eprintln!(
                "  ({} settled after {} redraws)",
                open.name,
                open.drawn.saturating_sub(1)
            );
        });
        STATE.with_borrow_mut(|state| {
            if let Some(run) = state.as_mut() {
                report_marks(&run.marks);
                run.marks.clear();
            }
        });
    });
}

/// The open channel now shows `rows` rows.
///
/// Called every time the list is spliced, which is once for the click
/// itself and again for whatever arrives afterwards.
pub(crate) fn switch_drew(channel: &str, name: &str, rows: usize) {
    if !watching_switch() {
        return;
    }
    SWITCH.with_borrow_mut(|switch| {
        let Some(open) = switch.as_mut() else {
            return;
        };
        if open.channel != channel {
            return;
        }
        name.clone_into(&mut open.name);
        let since = open.began.elapsed().as_secs_f64() * 1000.0;
        if open.drawn == 0 {
            eprintln!("switch {name}: {rows} rows from local state after {since:.0}ms");
        } else if open.drawn <= CHANGES_SHOWN {
            eprintln!("  {name}: now {rows} rows, {since:.0}ms after the click");
        }
        open.drawn += 1;
    });
}
