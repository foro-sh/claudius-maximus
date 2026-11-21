//! Telling a dead `claude` login apart from an ordinary failed run.
//!
//! A run that fails because the OAuth session expired fails the same way for
//! every issue on the box, in about a second, for as long as nobody logs in
//! again. Handled as an ordinary per-issue failure it costs one run per issue
//! and a backoff that outlives the fix, so the worker has to know it is not
//! the issue's fault.
//!
//! Same rules as [`crate::limits`]: the CLI's wording is not an API, so this is
//! a cheap guess and is arranged so that guessing wrong is cheap too. It only
//! looks at the end of a run that *failed* (an implement run whose own output
//! talks about OAuth has not lost its login), and it does not decide anything
//! alone: the worker stays paused only until a probe run proves the login
//! works, so a false positive costs one sweep and a false negative is today's
//! per-issue behaviour, now with the reason in the journal.

use std::fmt;

/// Phrases that mean the login is dead, matched case-insensitively.
const DEAD: &[&str] = &[
    "failed to authenticate",
    "oauth session expired",
    "could not be refreshed",
    "invalid api key",
    "please run /login",
];

/// How many lines at the end of each stream are looked at.
const TAIL_LINES: usize = 5;

/// How much of the matching line is kept: it is untrusted output going onto a
/// status page and into a chat room.
const SAID_MAX: usize = 200;

/// A run failed because `claude` cannot authenticate. Carried as the error
/// itself so that whoever handles the failure can `downcast_ref` for it, which
/// only works while nothing in between turns the error into a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure {
    /// The line that gave it away.
    pub said: String,
    /// The whole failure message, as an ordinary failed run would report it.
    pub message: String,
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "claude cannot authenticate ({})", self.message)
    }
}

impl std::error::Error for AuthFailure {}

/// The line that says the login is dead, if one of the last few of `lines`
/// does. Call it on a run that failed, never on one that succeeded.
pub fn classify(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .take(TAIL_LINES)
        .find(|line| {
            let line = line.to_ascii_lowercase();
            DEAD.iter().any(|phrase| line.contains(phrase))
        })
        .map(|line| line.trim().chars().take(SAID_MAX).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn the_incident_line_is_recognised_in_any_case() {
        let line = "Failed to authenticate: OAuth session expired and could not be refreshed";
        assert_eq!(classify(&lines(line)).as_deref(), Some(line));
        assert!(classify(&lines(&line.to_uppercase())).is_some());
    }

    #[test]
    fn ordinary_failures_are_not_auth_failures() {
        assert_eq!(classify(&lines("error: could not compile `foo`")), None);
        assert_eq!(
            classify(&lines("Claude AI usage limit reached|1763481600")),
            None
        );
        assert_eq!(classify(&[]), None);
    }

    #[test]
    fn only_the_end_of_the_output_counts() {
        let mut out = lines("this issue is about OAuth session expired errors");
        out.extend((0..TAIL_LINES).map(|n| format!("line {n}")));
        assert_eq!(classify(&out), None, "buried further up than the tail");
    }

    #[test]
    fn the_line_is_clipped() {
        let long = format!("Failed to authenticate: {}", "x".repeat(1000));
        assert_eq!(classify(&lines(&long)).unwrap().chars().count(), SAID_MAX);
    }
}
