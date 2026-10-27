//! Where a downloaded attachment goes, and under what name.
//!
//! **Pure, and inside the coverage gate.** A file name on a message is
//! attacker-controlled: it comes off the wire, anybody in the channel
//! chose it, and `../../../.bashrc` is a perfectly good file name. The
//! decision about what to call the thing and where to put it is
//! therefore a decision worth testing, and it is made here; the crate
//! that writes bytes to disk only carries it out.

use std::path::{Path, PathBuf};

/// What to call a downloaded file on disk.
///
/// Reduced to its last component with the separators taken out, so
/// nothing a sender writes can point outside the folder it is being
/// saved into. A name that reduces to nothing -- `..`, `/`, a string of
/// dots -- becomes `attachment`, because refusing to save it would be a
/// worse answer than saving it somewhere obvious.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && *c != '\0')
        .collect();
    // Trailing dots and spaces only. A *leading* dot is the whole name
    // of a hidden file -- `.bashrc` saved as `bashrc` would be a
    // different file -- and it is harmless once the separators are
    // gone, because a name with no separator in it cannot point
    // anywhere but inside the folder.
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    if cleaned.is_empty() {
        "attachment".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// The path to save under, given what is already in the folder.
///
/// Saving the same screenshot twice must not overwrite the first one,
/// so a name already taken grows a counter the way every browser does
/// it: `shot.png`, `shot (1).png`, `shot (2).png`. `taken` answers
/// whether a path is in use, which is what keeps this testable without
/// a filesystem.
pub fn destination(folder: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let name = safe_name(name);
    let first = folder.join(&name);
    if !taken(&first) {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        // A leading dot is the whole name of a hidden file, not an
        // extension, so `.bashrc` stays `.bashrc (1)`.
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name.as_str(), String::new()),
    };
    // Bounded so a folder somebody has filled with every variant cannot
    // spin here; the last one is then overwritten, which is a better
    // answer than hanging.
    for n in 1..1_000 {
        let candidate = folder.join(format!("{stem} ({n}){extension}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    folder.join(format!("{stem} (999){extension}"))
}

#[cfg(test)]
#[path = "tests/download.rs"]
mod tests;
