//! Settings that the server proposes and the user may override.
//!
//! Two of them decide how every message looks, and both were being guessed.
//! Neither is a single value: the server states a default in its client
//! config, the user may hold a preference that overrides it, and for one of
//! them the server may forbid the override. Getting the precedence wrong is
//! invisible in testing against a default server and glaring against a real
//! one — the names in the sidebar simply do not match what the rest of the
//! organisation sees.

use crate::unread::Threading;
use mm_types::NameFormat;

/// Which name to show for a person.
///
/// `locked` is the server's `LockTeammateNameDisplay`: when it is on, the
/// administrator has decided for everyone and the preference is ignored.
#[must_use]
pub fn name_format(config: Option<&str>, locked: bool, preference: Option<&str>) -> NameFormat {
    let chosen = match preference {
        Some(value) if !locked && !value.is_empty() => value,
        // No preference, or the admin locked it: the server's default.
        _ => config.unwrap_or_default(),
    };
    NameFormat::from_value(chosen)
}

/// Whether collapsed reply threads are in effect.
///
/// The server's four values are not a setting but a *policy*:
/// `disabled` and `always_on` decide outright, while `default_on` and
/// `default_off` only say which way the user's preference falls when they
/// have not expressed one. Treating `default_on` as "on" regardless — which
/// is what this client did — hides every reply from a user who deliberately
/// turned threads off.
#[must_use]
pub fn threading(config: Option<&str>, preference: Option<&str>) -> Threading {
    match config.unwrap_or_default() {
        "always_on" => Threading::Collapsed,
        "disabled" => Threading::Flat,
        other => {
            let default_on = other == "default_on";
            let on = match preference {
                Some("on" | "true") => true,
                Some("off" | "false") => false,
                _ => default_on,
            };
            if on {
                Threading::Collapsed
            } else {
                Threading::Flat
            }
        }
    }
}

/// How long after posting a message may still be edited, from the
/// server's `PostEditTimeLimit`.
///
/// Seconds on the wire, as a string like everything in the client
/// config; `-1` is the default and means no limit, and so does anything
/// unreadable, because refusing to offer an edit the server would have
/// taken is the worse mistake -- the server still has the last word.
#[must_use]
pub fn edit_limit(config: Option<&str>) -> Option<std::time::Duration> {
    let seconds: i64 = config?.trim().parse().ok()?;
    u64::try_from(seconds)
        .ok()
        .map(std::time::Duration::from_secs)
}

/// Whether a message posted at `created` may still be edited at `now`,
/// under a limit from [`edit_limit`]. The server compares the same two
/// moments; this only spares the reader a refused request.
#[must_use]
pub fn editable(
    limit: Option<std::time::Duration>,
    created: mm_types::Timestamp,
    now: mm_types::Timestamp,
) -> bool {
    let Some(limit) = limit else {
        return true;
    };
    // A clock behind the server's makes a message from the future, which
    // is as young as a message gets.
    let age = u64::try_from(now.millis().saturating_sub(created.millis())).unwrap_or(0);
    u128::from(age) <= limit.as_millis()
}

#[cfg(test)]
#[path = "tests/settings.rs"]
mod tests;
