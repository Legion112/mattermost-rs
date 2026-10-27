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

#[cfg(test)]
#[path = "tests/wrap.rs"]
mod tests;
