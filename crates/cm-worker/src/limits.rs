//! Reading the usage window out of what `claude` says.
//!
//! The worker does not manage the window and must not try to: `claude` itself
//! waits it out (`CLAUDE_CODE_RETRY_WATCHDOG=1`) and resumes the run it was in
//! the middle of, which is why a spent window costs the backlog time and
//! nothing else. What was missing is that nobody could *tell*: a worker parked
//! on a reset looks exactly like a hung one from outside, for up to five
//! hours.
//!
//! So this module reads, and only reads. Every line of a run goes past
//! [`classify`], and what it recognises turns into a status change, a journal
//! line and one Mattermost message. Recognising nothing changes no behaviour
//! at all: the run still waits, still resumes, still ships. That is
//! deliberate. The `claude` CLI's wording is not an API, so this is the one
//! place in the worker allowed to be a best guess, and it is arranged so that
//! guessing wrong costs only the telling.

/// What a line of `claude` output says about the usage window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// The window is spent. The run is not dead: it is waiting for the reset.
    Spent(Spent),
    /// The window is open again and the run is moving.
    Resumed,
    /// Nearly spent. Worth saying once, since it means the run in flight may
    /// be the last one for a few hours.
    Approaching,
}

/// When a spent window reopens, as far as the line said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spent {
    /// Unix seconds, from the `...|1763481600` form the CLI uses.
    pub resets_at: Option<u64>,
    /// What the line said about the reset in words, kept verbatim for the
    /// humans reading the status page. Capped: this is untrusted output being
    /// put on a page and into a chat room.
    pub said: Option<String>,
}

/// How much of a reset phrase is kept.
const SAID_MAX: usize = 120;

/// Phrases that mean the window is spent.
const SPENT: &[&str] = &[
    "usage limit reached",
    "reached your usage limit",
    "you've hit your usage limit",
    "5-hour limit reached",
    "five-hour limit reached",
    "out of usage",
    "quota exhausted",
    "rate limit reached",
    "waiting for your usage limit to reset",
];

/// Phrases that mean it is open again. The watchdog's own wording is the one
/// this is least sure of, so [`crate::worker`] also treats a run that finishes
/// as proof the window reopened.
const RESUMED: &[&str] = &[
    "usage limit reset",
    "usage limit has reset",
    "limit has reset",
    "window reset",
    "resuming after",
    "retrying now",
    "back within your usage limit",
];

/// Phrases that mean it is nearly spent.
const APPROACHING: &[&str] = &[
    "approaching your usage limit",
    "approaching the usage limit",
    "approaching usage limit",
    "running low on usage",
];

/// What this line says about the window, if anything.
///
/// Order matters: "approaching your usage limit" contains none of the spent
/// phrases, but a reset line often names the limit too, so the two narrower
/// sets are tried before the broad one.
pub fn classify(line: &str) -> Option<Signal> {
    let haystack = line.to_ascii_lowercase();
    let says = |set: &[&str]| set.iter().any(|phrase| haystack.contains(phrase));

    if says(APPROACHING) {
        return Some(Signal::Approaching);
    }
    if says(RESUMED) {
        return Some(Signal::Resumed);
    }
    if says(SPENT) {
        return Some(Signal::Spent(Spent {
            resets_at: reset_epoch(line),
            said: reset_said(line),
        }));
    }
    None
}

/// The epoch the CLI appends to its own limit message, as in
/// `Claude AI usage limit reached|1763481600`.
///
/// Anything that is not a plausible epoch is dropped rather than shown: a
/// status page saying the window reopens in 1970 is worse than one saying it
/// does not know.
fn reset_epoch(line: &str) -> Option<u64> {
    let digits: String = line
        .rsplit('|')
        .next()?
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if !line.contains('|') || digits.is_empty() {
        return None;
    }
    let epoch: u64 = digits.parse().ok()?;
    // Sometime after this project existed, and not so far out that a
    // millisecond timestamp read as seconds sails through.
    (1_700_000_000..4_000_000_000)
        .contains(&epoch)
        .then_some(epoch)
}

/// Whatever the line said in words about the reset: everything from the word
/// "reset" to the end, which is where the CLI puts the time and the timezone.
fn reset_said(line: &str) -> Option<String> {
    let at = line.to_ascii_lowercase().find("reset")?;
    let said: String = line[at..]
        .split('|')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(['.', ')'])
        .chars()
        .take(SAID_MAX)
        .collect();
    (!said.is_empty()).then_some(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spent(line: &str) -> Spent {
        match classify(line) {
            Some(Signal::Spent(spent)) => spent,
            other => panic!("expected a spent window for {line:?}, got {other:?}"),
        }
    }

    #[test]
    fn the_clis_own_limit_line_carries_the_reset_epoch() {
        assert_eq!(
            spent("Claude AI usage limit reached|1763481600"),
            Spent {
                resets_at: Some(1_763_481_600),
                said: None,
            }
        );
    }

    #[test]
    fn a_limit_line_in_words_keeps_what_it_said() {
        assert_eq!(
            spent("Claude usage limit reached. Your limit will reset at 3pm (Europe/Amsterdam)."),
            Spent {
                resets_at: None,
                said: Some("reset at 3pm (Europe/Amsterdam".to_string()),
            }
        );
    }

    #[test]
    fn casing_and_surrounding_noise_do_not_matter() {
        assert!(matches!(
            classify("  [warn] 5-Hour Limit Reached, waiting  "),
            Some(Signal::Spent(_))
        ));
    }

    #[test]
    fn a_reset_line_is_a_resume_not_a_new_limit() {
        assert_eq!(
            classify("usage limit reset, retrying the request"),
            Some(Signal::Resumed)
        );
    }

    #[test]
    fn approaching_is_not_spent() {
        assert_eq!(
            classify("Warning: approaching your usage limit"),
            Some(Signal::Approaching)
        );
    }

    #[test]
    fn ordinary_output_says_nothing_about_the_window() {
        assert_eq!(classify("## Plan"), None);
        assert_eq!(classify("running cargo test --workspace"), None);
        assert_eq!(
            classify("the limit is 128KiB per argv entry"),
            None,
            "a plan that talks about limits is not a limit notice"
        );
    }

    #[test]
    fn an_implausible_epoch_is_dropped_rather_than_shown() {
        assert_eq!(spent("usage limit reached|12").resets_at, None);
        assert_eq!(
            spent("usage limit reached|1763481600000").resets_at,
            None,
            "milliseconds read as seconds would land in the year 57000"
        );
        assert_eq!(spent("usage limit reached|tomorrow").resets_at, None);
    }
}
