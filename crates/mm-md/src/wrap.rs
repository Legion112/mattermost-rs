//! Putting markdown markers round a selection, and taking them off.
//!
//! The keyboard shortcuts a composer offers -- bold, italic, code --
//! are all the same operation with a different marker, and all of it
//! is arithmetic on character offsets. Pure, so the awkward cases
//! (nothing selected, already wrapped, wrapped on the outside) are
//! settled by tests rather than by trying it in the window.

/// What the box should say afterwards, and what should be selected.
///
/// Offsets are in **characters**, like a `GtkTextBuffer`'s, not bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Wrap the selection in `marker`, or take `marker` off if it is
/// already there.
///
/// A toggle, the way every editor does it: pressing bold twice leaves
/// what you started with. The markers are looked for **outside** the
/// selection first -- selecting a word inside `**word**` and pressing
/// bold is the common way to unbold it -- and then inside, which is
/// what a selection made by double-clicking through the markers
/// gives.
///
/// With nothing selected, the markers go in and the caret lands
/// between them, ready to type.
#[must_use]
pub fn wrap(text: &str, start: usize, end: usize, marker: &str) -> Wrapped {
    let chars: Vec<char> = text.chars().collect();
    let marks: Vec<char> = marker.chars().collect();
    let width = marks.len();
    let (start, end) = (start.min(chars.len()), end.min(chars.len()));
    let (start, end) = (start.min(end), start.max(end));

    let at = |from: usize| -> bool {
        from + width <= chars.len() && chars[from..from + width] == marks[..]
    };
    let join = |pieces: &[&[char]]| -> String { pieces.concat().into_iter().collect() };

    // Already wrapped from outside: `**|word|**`.
    if start >= width && at(start - width) && at(end) {
        return Wrapped {
            text: join(&[
                &chars[..start - width],
                &chars[start..end],
                &chars[end + width..],
            ]),
            start: start - width,
            end: end - width,
        };
    }
    // Already wrapped inside the selection: `|**word**|`.
    if end >= start + 2 * width && at(start) && at(end - width) {
        return Wrapped {
            text: join(&[
                &chars[..start],
                &chars[start + width..end - width],
                &chars[end..],
            ]),
            start,
            end: end - 2 * width,
        };
    }
    Wrapped {
        text: join(&[
            &chars[..start],
            &marks,
            &chars[start..end],
            &marks,
            &chars[end..],
        ]),
        start: start + width,
        end: end + width,
    }
}

/// Put `prefix` at the start of every line the selection touches, or
/// take it off every one of them when they all already have it.
///
/// A list, a quote, a heading: each is a prefix on a line, and the
/// toolbar button for each is a toggle, like the markers. A selection
/// that ends exactly at a line break does not touch the line after it.
#[must_use]
pub fn prefix_lines(text: &str, start: usize, end: usize, prefix: &str) -> Wrapped {
    let marks: Vec<char> = prefix.chars().collect();
    reline(
        text,
        start,
        end,
        |line| {
            (line.len() >= marks.len() && line[..marks.len()] == marks[..]).then_some(marks.len())
        },
        |_| prefix.to_owned(),
    )
}

/// Number the lines the selection touches `1.`, `2.`, `3.`, or take the
/// numbers off when every line has one. Lines numbered already are
/// renumbered, so a list with a line added in the middle comes out in
/// order.
#[must_use]
pub fn number_lines(text: &str, start: usize, end: usize) -> Wrapped {
    reline(text, start, end, numbered_width, |n| format!("{}. ", n + 1))
}

/// The width of a `12. ` prefix on a line, when it has one.
fn numbered_width(line: &[char]) -> Option<usize> {
    let digits = line.iter().take_while(|c| c.is_ascii_digit()).count();
    (digits > 0 && line.get(digits) == Some(&'.') && line.get(digits + 1) == Some(&' '))
        .then_some(digits + 2)
}

/// Make the selection a link: `[what was selected](url)` with `url`
/// selected, ready to be typed over -- or, with nothing selected,
/// `[text](url)` with `text` selected.
#[must_use]
pub fn link(text: &str, start: usize, end: usize) -> Wrapped {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = (start.min(chars.len()), end.min(chars.len()));
    let (start, end) = (start.min(end), start.max(end));
    let before: String = chars[..start].iter().collect();
    let after: String = chars[end..].iter().collect();
    if start == end {
        return Wrapped {
            text: format!("{before}[text](url){after}"),
            start: start + 1,
            end: start + 5,
        };
    }
    let chosen: String = chars[start..end].iter().collect();
    Wrapped {
        text: format!("{before}[{chosen}](url){after}"),
        start: end + 3,
        end: end + 6,
    }
}

/// The shared arithmetic of the line-prefix toggles.
///
/// `has` says how wide a line's existing prefix is, when it has one;
/// `make` gives the prefix for the n-th touched line. When every
/// touched line has a prefix they all come off; otherwise every line
/// gets one, replacing whatever prefix it had. The selection moves
/// with its text: the start by the first line's change, the end by
/// all of them.
fn reline(
    text: &str,
    start: usize,
    end: usize,
    has: impl Fn(&[char]) -> Option<usize>,
    make: impl Fn(usize) -> String,
) -> Wrapped {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = (start.min(chars.len()), end.min(chars.len()));
    let (start, end) = (start.min(end), start.max(end));
    // A selection ending just after a line break stops at that break.
    let last = if end > start && chars[end - 1] == '\n' {
        end - 1
    } else {
        end
    };
    let block_start = chars[..start]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |at| at + 1);
    let block_end = chars[last..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |at| last + at);
    let lines: Vec<&[char]> = chars[block_start..block_end]
        .split(|c| *c == '\n')
        .collect();
    let widths: Vec<Option<usize>> = lines.iter().map(|line| has(line)).collect();
    let removing = widths.iter().all(Option::is_some);

    let mut out: Vec<char> = chars[..block_start].to_vec();
    let mut first_shift: isize = 0;
    let mut total_shift: isize = 0;
    for (n, (line, width)) in lines.iter().zip(&widths).enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let had = width.unwrap_or(0);
        let added: Vec<char> = if removing {
            Vec::new()
        } else {
            make(n).chars().collect()
        };
        let shift = isize::try_from(added.len()).unwrap_or(0) - isize::try_from(had).unwrap_or(0);
        if n == 0 {
            first_shift = shift;
        }
        total_shift += shift;
        out.extend(added);
        out.extend_from_slice(&line[had..]);
    }
    out.extend_from_slice(&chars[block_end..]);

    // A caret inside a prefix that has just gone lands where the
    // prefix was, not before the line.
    let moved = |at: usize, by: isize, floor: usize| -> usize {
        usize::try_from(isize::try_from(at).unwrap_or(0) + by)
            .unwrap_or(0)
            .max(floor)
    };
    let new_start = moved(start, first_shift, block_start);
    let new_end = moved(end, total_shift, new_start);
    Wrapped {
        text: out.into_iter().collect(),
        start: new_start,
        end: new_end,
    }
}

#[cfg(test)]
#[path = "tests/wrap.rs"]
mod tests;
