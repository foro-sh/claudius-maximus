//! The register: what the worker is doing right now, what it has done since it
//! rose, and whether the usage window is open.
//!
//! Every other part of the worker reports *into* here, and the three things
//! that report *out* (the journal heartbeat, systemd's `STATUS=`, and the HTTP
//! page) read one [`Snapshot`] instead of reaching into live state. That seam
//! is the whole design: a snapshot is a plain value with no clocks and no lock
//! in it, so rendering it is a pure function and every renderer is testable
//! without a socket, a sweep, or a five-hour wait.
//!
//! In memory, like [`crate::backoff`] and for the same reason: GitHub holds
//! the state that matters. This is a window onto a running process, and a
//! process that restarted has nothing to remember.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::limits::Spent;

/// How many recent events the register keeps. Enough to cover a long sweep's
/// worth of news on the status page, few enough that the whole thing stays a
/// couple of kilobytes of JSON.
const EVENTS_KEPT: usize = 64;

/// The longest line of `claude` output kept for display. The output is
/// untrusted (an issue body wrote most of the prompt that produced it) and it
/// ends up on a web page and in a chat room, so it is cut here, once.
const LINE_MAX: usize = 200;

/// What the worker is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Before the first sweep: reading config, claiming the label.
    Rising,
    /// Blocked on the GitHub device flow, which is a human's move to make.
    Authorizing,
    /// Walking a repo's backlog.
    Sweeping,
    /// A `claude` run, writing a plan.
    Planning,
    /// A `claude` run, writing the code.
    Implementing,
    /// Pushing a branch and opening its PR.
    Shipping,
    /// Between sweeps.
    Resting,
}

impl Stage {
    /// True for the stages that have a `claude` process in flight. These are
    /// the ones allowed to take hours: everything that watches for a stuck
    /// worker has to know the difference.
    pub fn runs_claude(self) -> bool {
        matches!(self, Stage::Planning | Stage::Implementing)
    }

    /// The word used in logs, in `STATUS=`, on the page and as a metric label.
    pub fn word(self) -> &'static str {
        match self {
            Stage::Rising => "rising",
            Stage::Authorizing => "authorizing",
            Stage::Sweeping => "sweeping",
            Stage::Planning => "planning",
            Stage::Implementing => "implementing",
            Stage::Shipping => "shipping",
            Stage::Resting => "resting",
        }
    }
}

/// Every stage there is, for the renderers that want one series per stage
/// rather than one value naming the current one.
pub const STAGES: [Stage; 7] = [
    Stage::Rising,
    Stage::Authorizing,
    Stage::Sweeping,
    Stage::Planning,
    Stage::Implementing,
    Stage::Shipping,
    Stage::Resting,
];

/// A stage, and what it is being done to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub stage: Stage,
    /// `foro-sh/foro#7`, or a repo name, or nothing.
    pub subject: Option<String>,
    /// The model, for the stages that run one.
    pub model: Option<String>,
}

impl Activity {
    pub fn bare(stage: Stage) -> Self {
        Activity {
            stage,
            subject: None,
            model: None,
        }
    }

    pub fn on(stage: Stage, subject: impl Into<String>) -> Self {
        Activity {
            stage,
            subject: Some(subject.into()),
            model: None,
        }
    }

    pub fn run(stage: Stage, subject: impl Into<String>, model: impl Into<String>) -> Self {
        Activity {
            stage,
            subject: Some(subject.into()),
            model: Some(model.into()),
        }
    }
}

/// The usage window, as far as the worker can tell from what `claude` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Window {
    Open,
    Shut {
        for_: Duration,
        /// Unix seconds, when the CLI said so.
        resets_at: Option<u64>,
        /// What the CLI said about the reset, in words.
        said: Option<String>,
    },
}

impl Window {
    pub fn is_shut(&self) -> bool {
        matches!(self, Window::Shut { .. })
    }
}

/// Everything counted since the worker rose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub sweeps: u64,
    pub issues_seen: u64,
    pub plans_posted: u64,
    pub pulls_opened: u64,
    pub claude_runs: u64,
    pub claude_failures: u64,
    pub issue_failures: u64,
    pub repo_failures: u64,
    pub windows_shut: u64,
    /// Wall time inside a `claude` process, which on a healthy instance is
    /// most of the wall time there is.
    pub claude_time: Duration,
    /// Wall time spent waiting out a spent window: the throughput ceiling,
    /// measured instead of guessed.
    pub window_time: Duration,
}

/// What one sweep did, which is the line worth reading in the journal when
/// nothing went wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub seen: u64,
    pub planned: u64,
    pub shipped: u64,
    pub skipped: u64,
    pub resting: u64,
    pub failed: u64,
}

impl Tally {
    pub fn line(&self, took: Duration) -> String {
        format!(
            "sweep done in {}: {} issue(s) seen, {} planned, {} shipped, {} skipped, {} resting, \
             {} failed",
            human(took),
            self.seen,
            self.planned,
            self.shipped,
            self.skipped,
            self.resting,
            self.failed
        )
    }
}

/// A finished sweep, as the page and `/healthz` see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSweep {
    pub tally: Tally,
    pub took: Duration,
    pub ended_epoch: u64,
    pub ended_ago: Duration,
}

/// How loud one line of the worker's own history is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// It happened.
    Note,
    /// It went well: a plan, a PR.
    Good,
    /// It did not: a failure, a spent window.
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub at_epoch: u64,
    pub level: Level,
    pub text: String,
}

/// A consistent view of the register, taken under one lock and then owned by
/// whoever asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub instance: String,
    pub label: String,
    pub repos: Vec<String>,
    pub poll_interval: Duration,
    pub started_epoch: u64,
    pub now_epoch: u64,
    pub uptime: Duration,
    pub activity: Activity,
    /// How long the current activity has lasted.
    pub activity_for: Duration,
    pub window: Window,
    /// How long since `claude` last wrote a line, while a run is in flight.
    /// This is the number that separates a run that is working from a run that
    /// is waiting.
    pub quiet_for: Option<Duration>,
    pub last_line: Option<String>,
    pub counters: Counters,
    pub last_sweep: Option<LastSweep>,
    pub events: Vec<Event>,
}

impl Snapshot {
    /// The one line the heartbeat logs and systemd shows. Written to be read
    /// at a glance in `systemctl status`, so the subject comes first and the
    /// elapsed time comes with it: "implementing foro-sh/foro#7 for 12m".
    pub fn line(&self) -> String {
        if let Window::Shut {
            for_,
            resets_at,
            said,
        } = &self.window
        {
            let when = match (resets_at, said) {
                (Some(epoch), _) => format!(", reopens in {}", human(self.until(*epoch))),
                (None, Some(said)) => format!(", claude said: {said}"),
                (None, None) => String::new(),
            };
            return format!(
                "waiting out a spent usage window for {}{when} (was {})",
                human(*for_),
                self.doing()
            );
        }
        let mut line = self.doing();
        line.push_str(&format!(" for {}", human(self.activity_for)));
        if let Some(quiet) = self.quiet_for {
            line.push_str(&format!(", quiet for {}", human(quiet)));
        }
        line
    }

    /// "implementing foro-sh/foro#7 (claude-sonnet-5)", without the times.
    pub fn doing(&self) -> String {
        let mut doing = self.activity.stage.word().to_string();
        if let Some(subject) = &self.activity.subject {
            doing.push(' ');
            doing.push_str(subject);
        }
        if let Some(model) = &self.activity.model {
            doing.push_str(&format!(" ({model})"));
        }
        doing
    }

    /// How long until an epoch, from the moment the snapshot was taken. Zero
    /// once it is in the past, which is the honest answer for a reset time
    /// that has come and gone without the run saying anything.
    pub fn until(&self, epoch: u64) -> Duration {
        Duration::from_secs(epoch.saturating_sub(self.now_epoch))
    }
}

pub struct Status {
    instance: String,
    label: String,
    repos: Vec<String>,
    poll_interval: Duration,
    started: Instant,
    started_epoch: u64,
    inner: Mutex<Inner>,
}

struct Inner {
    activity: Activity,
    activity_since: Instant,
    window: Shutter,
    counters: Counters,
    last_line: Option<String>,
    last_line_at: Option<Instant>,
    last_sweep: Option<(Tally, Duration, u64, Instant)>,
    events: VecDeque<Event>,
    /// Whether "nearly spent" has been said since the window last changed.
    /// The CLI repeats that warning as freely as it repeats the limit itself,
    /// and the chronicle is 64 entries deep: unchecked, one talkative run
    /// evicts every real event in it.
    said_nearly: bool,
}

/// The live half of [`Window`]: `Instant` rather than a duration, since a
/// duration would have to be recomputed on every read anyway.
enum Shutter {
    Open,
    Shut {
        since: Instant,
        resets_at: Option<u64>,
        said: Option<String>,
    },
}

impl Status {
    pub fn new(config: &Config) -> Self {
        Status {
            instance: config.instance.clone(),
            label: config.label.clone(),
            repos: config.repos.iter().map(|r| r.repo.clone()).collect(),
            poll_interval: config.poll_interval,
            started: Instant::now(),
            started_epoch: now_epoch(),
            inner: Mutex::new(Inner {
                activity: Activity::bare(Stage::Rising),
                activity_since: Instant::now(),
                window: Shutter::Open,
                counters: Counters::default(),
                last_line: None,
                last_line_at: None,
                last_sweep: None,
                events: VecDeque::new(),
                said_nearly: false,
            }),
        }
    }

    /// Moves to a new activity. The clock on "how long has this been going"
    /// restarts here, and so does the one on `claude`'s silence: a new stage
    /// has not been quiet, it has not started.
    pub fn doing(&self, activity: Activity) {
        let mut inner = self.inner.lock().unwrap();
        inner.activity = activity;
        inner.activity_since = Instant::now();
        inner.last_line = None;
        inner.last_line_at = None;
    }

    /// A line of `claude` output. Kept as the answer to "is this run alive",
    /// which is the question a quiet hour actually raises.
    pub fn heard(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.last_line = Some(clip(line, LINE_MAX));
        inner.last_line_at = Some(Instant::now());
    }

    /// Remembers something worth reading back later.
    pub fn note(&self, level: Level, text: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.events.push_back(Event {
            at_epoch: now_epoch(),
            level,
            text: clip(&text.into(), LINE_MAX),
        });
        while inner.events.len() > EVENTS_KEPT {
            inner.events.pop_front();
        }
    }

    /// The window is spent. Idempotent: the CLI says so more than once while
    /// it waits, and the thing being measured is how long the *window* has
    /// been shut, not how long ago the last complaint was.
    ///
    /// Returns true the first time, which is what makes the caller's log line
    /// and its Mattermost message happen once.
    pub fn window_shut(&self, spent: &Spent) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Shutter::Shut {
            resets_at, said, ..
        } = &mut inner.window
        {
            // A later line may know the reset time when the first did not.
            *resets_at = resets_at.or(spent.resets_at);
            *said = said.clone().or_else(|| spent.said.clone());
            return false;
        }
        inner.window = Shutter::Shut {
            since: Instant::now(),
            resets_at: spent.resets_at,
            said: spent.said.clone(),
        };
        inner.said_nearly = false;
        inner.counters.windows_shut += 1;
        true
    }

    /// The window is nearly spent. True the first time since the window last
    /// changed, and false for every repeat of the same warning.
    pub fn window_nearly(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        !std::mem::replace(&mut inner.said_nearly, true)
    }

    /// Reopens a window that cannot still be shut: the reset time `claude`
    /// gave has come and gone, or it has been shut longer than any window
    /// lasts.
    ///
    /// Without this the shut state latches. Nothing but a run coming back
    /// clears it, and a run cannot come back while every run fails for some
    /// other reason (expired credentials, say), so the page would say "waiting
    /// out a spent usage window" for the life of the process. Returns how long
    /// it was shut, when it reopened one.
    pub fn window_expired(&self, ceiling: Duration) -> Option<Duration> {
        let expired = {
            let inner = self.inner.lock().unwrap();
            match inner.window {
                Shutter::Open => false,
                Shutter::Shut {
                    since, resets_at, ..
                } => {
                    since.elapsed() > ceiling || resets_at.is_some_and(|epoch| epoch < now_epoch())
                }
            }
        };
        expired.then(|| self.window_open()).flatten()
    }

    /// The window is open again. Returns how long it was shut, or `None` if it
    /// was never shut, so the caller can say "back after 2h11m" only when
    /// there was something to be back from.
    pub fn window_open(&self) -> Option<Duration> {
        let mut inner = self.inner.lock().unwrap();
        let Shutter::Shut { since, .. } = inner.window else {
            return None;
        };
        let shut_for = since.elapsed();
        inner.counters.window_time += shut_for;
        inner.window = Shutter::Open;
        inner.said_nearly = false;
        Some(shut_for)
    }

    /// One finished `claude` run, however it went.
    pub fn ran_claude(&self, took: Duration, ok: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.counters.claude_runs += 1;
        inner.counters.claude_time += took;
        if !ok {
            inner.counters.claude_failures += 1;
        }
    }

    pub fn planned(&self) {
        self.inner.lock().unwrap().counters.plans_posted += 1;
    }

    pub fn shipped(&self) {
        self.inner.lock().unwrap().counters.pulls_opened += 1;
    }

    pub fn issue_failed(&self) {
        self.inner.lock().unwrap().counters.issue_failures += 1;
    }

    pub fn repo_failed(&self) {
        self.inner.lock().unwrap().counters.repo_failures += 1;
    }

    /// A sweep came round. The tally is the count the journal line reports and
    /// the page shows; the counters are the same numbers, added up.
    pub fn swept(&self, tally: Tally, took: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.counters.sweeps += 1;
        inner.counters.issues_seen += tally.seen;
        inner.last_sweep = Some((tally, took, now_epoch(), Instant::now()));
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        Snapshot {
            instance: self.instance.clone(),
            label: self.label.clone(),
            repos: self.repos.clone(),
            poll_interval: self.poll_interval,
            started_epoch: self.started_epoch,
            now_epoch: now_epoch(),
            uptime: self.started.elapsed(),
            activity: inner.activity.clone(),
            activity_for: inner.activity_since.elapsed(),
            window: match &inner.window {
                Shutter::Open => Window::Open,
                Shutter::Shut {
                    since,
                    resets_at,
                    said,
                } => Window::Shut {
                    for_: since.elapsed(),
                    resets_at: *resets_at,
                    said: said.clone(),
                },
            },
            quiet_for: inner
                .activity
                .stage
                .runs_claude()
                .then(|| match inner.last_line_at {
                    Some(at) => at.elapsed(),
                    // Nothing heard yet: the run has been quiet since it
                    // started, which is exactly what the number should say.
                    None => inner.activity_since.elapsed(),
                }),
            last_line: inner.last_line.clone(),
            counters: inner.counters.clone(),
            last_sweep: inner
                .last_sweep
                .as_ref()
                .map(|(tally, took, epoch, at)| LastSweep {
                    tally: tally.clone(),
                    took: *took,
                    ended_epoch: *epoch,
                    ended_ago: at.elapsed(),
                }),
            events: inner.events.iter().cloned().collect(),
        }
    }
}

/// Seconds since the epoch. A worker whose clock is before 1970 has worse
/// problems than a wrong status page.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A duration as somebody reading a journal wants it: `45s`, `12m`, `3h04m`,
/// `2d03h`. Never more than two units, because the second one is already
/// noise.
pub fn human(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        3600..86400 => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d{:02}h", secs / 86400, (secs % 86400) / 3600),
    }
}

/// Cuts a string to `max` characters (not bytes: this text can be anything),
/// marking the cut so nobody reads a truncated line as a whole one.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_register_is_rising_and_has_done_nothing() {
        let snapshot = Status::new(&Config::sample()).snapshot();
        assert_eq!(snapshot.activity.stage, Stage::Rising);
        assert_eq!(snapshot.counters, Counters::default());
        assert_eq!(snapshot.window, Window::Open);
        assert_eq!(snapshot.quiet_for, None, "no run, no silence to measure");
    }

    #[test]
    fn a_run_in_flight_measures_how_long_claude_has_been_quiet() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(
            Stage::Implementing,
            "foro-sh/foro#7",
            "claude-sonnet-5",
        ));
        assert!(status.snapshot().quiet_for.is_some());

        status.heard("cargo test --workspace");
        let snapshot = status.snapshot();
        assert_eq!(
            snapshot.last_line.as_deref(),
            Some("cargo test --workspace")
        );
        assert!(snapshot.doing().contains("implementing foro-sh/foro#7"));
        assert!(snapshot.doing().contains("claude-sonnet-5"));
    }

    #[test]
    fn moving_on_forgets_the_last_run_s_last_line() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        status.heard("## Plan");
        status.doing(Activity::bare(Stage::Resting));

        let snapshot = status.snapshot();
        assert_eq!(snapshot.last_line, None);
        assert_eq!(snapshot.quiet_for, None);
    }

    #[test]
    fn a_spent_window_is_only_news_once_and_reopening_is_measured() {
        let status = Status::new(&Config::sample());
        let spent = Spent {
            resets_at: None,
            said: None,
        };

        assert!(status.window_shut(&spent), "the first line is the news");
        assert!(
            !status.window_shut(&spent),
            "the CLI complains repeatedly while it waits"
        );
        assert!(status.snapshot().window.is_shut());

        assert!(status.window_open().is_some());
        assert_eq!(status.snapshot().window, Window::Open);
        assert!(
            status.window_open().is_none(),
            "an open window cannot open again"
        );
        assert_eq!(status.snapshot().counters.windows_shut, 1);
    }

    #[test]
    fn a_later_line_fills_in_a_reset_time_the_first_one_did_not_know() {
        let status = Status::new(&Config::sample());
        status.window_shut(&Spent {
            resets_at: None,
            said: None,
        });
        status.window_shut(&Spent {
            resets_at: Some(1_763_481_600),
            said: Some("reset at 3pm".to_string()),
        });

        match status.snapshot().window {
            Window::Shut {
                resets_at, said, ..
            } => {
                assert_eq!(resets_at, Some(1_763_481_600));
                assert_eq!(said.as_deref(), Some("reset at 3pm"));
            }
            other => panic!("expected a shut window, got {other:?}"),
        }
    }

    #[test]
    fn nearly_spent_is_said_once_per_window() {
        let status = Status::new(&Config::sample());

        assert!(status.window_nearly());
        assert!(!status.window_nearly(), "the CLI repeats that warning too");

        status.window_shut(&Spent::default());
        assert!(
            status.window_nearly(),
            "a new window is a new warning worth hearing"
        );

        status.window_open();
        assert!(status.window_nearly());
    }

    #[test]
    fn a_window_whose_reset_time_has_passed_does_not_stay_shut() {
        let status = Status::new(&Config::sample());
        status.window_shut(&Spent {
            resets_at: Some(now_epoch() - 1),
            said: None,
        });

        assert!(status.snapshot().window.is_shut());
        assert!(
            status
                .window_expired(Duration::from_secs(6 * 3600))
                .is_some(),
            "the reset time it gave us has come and gone"
        );
        assert_eq!(status.snapshot().window, Window::Open);
        assert!(
            status
                .window_expired(Duration::from_secs(6 * 3600))
                .is_none()
        );
    }

    #[test]
    fn a_window_shut_longer_than_any_window_lasts_does_not_stay_shut() {
        let status = Status::new(&Config::sample());
        status.window_shut(&Spent::default());

        assert!(
            status
                .window_expired(Duration::from_secs(6 * 3600))
                .is_none(),
            "a window that was just shut is simply shut"
        );
        assert!(
            status.window_expired(Duration::ZERO).is_some(),
            "one that has outlasted every possible window is not"
        );
    }

    #[test]
    fn the_events_ring_keeps_the_latest_and_drops_the_oldest() {
        let status = Status::new(&Config::sample());
        for n in 0..EVENTS_KEPT + 5 {
            status.note(Level::Note, format!("event {n}"));
        }

        let events = status.snapshot().events;
        assert_eq!(events.len(), EVENTS_KEPT);
        assert_eq!(events[0].text, "event 5");
        assert_eq!(
            events[EVENTS_KEPT - 1].text,
            format!("event {}", EVENTS_KEPT + 4)
        );
    }

    #[test]
    fn counters_add_up_across_sweeps() {
        let status = Status::new(&Config::sample());
        status.ran_claude(Duration::from_secs(90), true);
        status.ran_claude(Duration::from_secs(30), false);
        status.planned();
        status.shipped();
        status.issue_failed();
        status.swept(
            Tally {
                seen: 3,
                planned: 1,
                shipped: 1,
                failed: 1,
                ..Tally::default()
            },
            Duration::from_secs(120),
        );

        let counters = status.snapshot().counters;
        assert_eq!(counters.claude_runs, 2);
        assert_eq!(counters.claude_failures, 1);
        assert_eq!(counters.claude_time, Duration::from_secs(120));
        assert_eq!(counters.plans_posted, 1);
        assert_eq!(counters.pulls_opened, 1);
        assert_eq!(counters.issue_failures, 1);
        assert_eq!(counters.sweeps, 1);
        assert_eq!(counters.issues_seen, 3);
    }

    #[test]
    fn the_status_line_leads_with_the_window_when_it_is_shut() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(
            Stage::Implementing,
            "foro-sh/foro#7",
            "claude-sonnet-5",
        ));
        status.window_shut(&Spent {
            resets_at: None,
            said: Some("reset at 3pm".to_string()),
        });

        let line = status.snapshot().line();
        assert!(
            line.starts_with("waiting out a spent usage window"),
            "{line}"
        );
        assert!(line.contains("reset at 3pm"), "{line}");
        assert!(
            line.contains("implementing foro-sh/foro#7"),
            "what it was doing when the window went is the point: {line}"
        );
    }

    #[test]
    fn a_reset_time_becomes_a_countdown() {
        let mut snapshot = Status::new(&Config::sample()).snapshot();
        snapshot.window = Window::Shut {
            for_: Duration::from_secs(60),
            resets_at: Some(snapshot.now_epoch + 3600),
            said: None,
        };
        assert!(
            snapshot.line().contains("reopens in 1h00m"),
            "{}",
            snapshot.line()
        );

        snapshot.window = Window::Shut {
            for_: Duration::from_secs(60),
            resets_at: Some(snapshot.now_epoch - 3600),
            said: None,
        };
        assert!(
            snapshot.line().contains("reopens in 0s"),
            "a reset time that has passed is not a negative countdown: {}",
            snapshot.line()
        );
    }

    #[test]
    fn durations_read_like_a_human_wrote_them() {
        assert_eq!(human(Duration::from_secs(9)), "9s");
        assert_eq!(human(Duration::from_secs(90)), "1m30s");
        assert_eq!(human(Duration::from_secs(3600 + 4 * 60)), "1h04m");
        assert_eq!(human(Duration::from_secs(2 * 86400 + 3 * 3600)), "2d03h");
    }

    #[test]
    fn an_endless_line_of_output_is_cut_and_marked() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        status.heard(&"x".repeat(LINE_MAX * 2));

        let line = status.snapshot().last_line.unwrap();
        assert_eq!(line.chars().count(), LINE_MAX + 1);
        assert!(line.ends_with('…'));
    }
}
