//! The pulse: one thread whose whole job is to say what the worker is doing
//! while the worker is too busy doing it to say anything.
//!
//! A `claude` run is minutes at best, an hour for an implement run, and hours
//! when it is waiting out a spent usage window. The sweep loop is blocked for
//! all of that, so anything that reports from inside it reports nothing until
//! it is over. Hence a thread, and deliberately an OS thread rather than a
//! tokio task: the run blocks a runtime worker thread, and on a one-core box a
//! task would be queued behind exactly the thing it is supposed to be
//! reporting on.
//!
//! It is only ever a reader. Nothing here can change what the worker does,
//! which is what makes it safe for it to run at any moment.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::notify::Notifier;
use crate::status::{Level, Snapshot, Status, human};
use crate::systemd::Systemd;
use crate::worker::stall_key;

pub struct Heartbeat {
    pub status: Arc<Status>,
    pub notifier: Arc<Notifier>,
    pub systemd: Arc<Systemd>,
    pub instance: String,
    /// How often to look. Every tick updates systemd and the watchdog; the
    /// journal hears less often than that, see [`Heartbeat::LOG_EVERY_TICKS`].
    pub every: Duration,
    /// How long a `claude` run may say nothing before that is worth a message.
    /// Zero turns that off.
    pub stall_after: Duration,
    /// No journal lines, only systemd. What `HEARTBEAT_INTERVAL=0` means under
    /// a unit with a watchdog: somebody asked for the old silence, and the
    /// watchdog still has to be answered or systemd kills a healthy worker
    /// every few minutes.
    pub quiet: bool,
}

/// What the thread carries between ticks, so that a beat that says the same
/// thing as the last one can stay quiet.
#[derive(Default)]
struct Beat {
    last_logged: Option<(String, Instant)>,
    stalled: Option<String>,
}

impl Heartbeat {
    /// Ticks between journal lines while nothing changes. The systemd status
    /// line and the watchdog are cheap and invisible, so they happen every
    /// tick; the journal gets one line per five, plus one whenever the worker
    /// moves on to something else.
    const LOG_EVERY_TICKS: u32 = 5;

    /// Starts beating. The thread runs until the process ends, which is what
    /// [`Heartbeat`] is for: there is no shutdown, there is a `systemctl
    /// stop`.
    pub fn start(mut self) {
        // Never slower than half the watchdog interval, whatever the config
        // says: systemd kills a worker whose pings stop, and a heartbeat that
        // beats too slowly is a worker that restarts itself every few minutes
        // for no reason.
        if let Some(watchdog) = self.systemd.watchdog_interval() {
            self.every = self.every.min(watchdog / 2).max(Duration::from_secs(1));
        }
        std::thread::Builder::new()
            .name("heartbeat".to_string())
            .spawn(move || {
                let mut beat = Beat::default();
                loop {
                    self.tick(&self.status.snapshot(), &mut beat);
                    std::thread::sleep(self.every);
                }
            })
            .expect("the heartbeat thread must start");
    }

    /// One beat, against a snapshot. Takes the snapshot rather than reading
    /// one so that every rule below is testable without a clock.
    fn tick(&self, snapshot: &Snapshot, beat: &mut Beat) {
        self.systemd.status(&snapshot.line());
        self.systemd.ping();

        // Silence is the point. A resting worker between sweeps is not news
        // once a minute; a run in flight, or a window being waited out, is
        // exactly what nobody could see before.
        if !self.quiet && (snapshot.activity.stage.runs_claude() || snapshot.window.is_shut()) {
            self.log_if_due(snapshot, beat);
        } else {
            beat.last_logged = None;
        }

        self.check_for_silence(snapshot, beat);
    }

    fn log_if_due(&self, snapshot: &Snapshot, beat: &mut Beat) {
        let doing = snapshot.doing();
        let due = match &beat.last_logged {
            Some((last, at)) => {
                *last != doing || at.elapsed() >= self.every * Self::LOG_EVERY_TICKS
            }
            None => true,
        };
        if due {
            println!("{}: {}", self.instance, snapshot.line());
            beat.last_logged = Some((doing, Instant::now()));
        }
    }

    /// A run that has not written a line in a long time is either thinking
    /// hard or gone. The worker cannot tell the difference and does not
    /// pretend to: it says how long the silence has lasted and leaves the run
    /// alone. Killing it would throw away an hour of work that is very
    /// probably still coming.
    fn check_for_silence(&self, snapshot: &Snapshot, beat: &mut Beat) {
        if self.stall_after.is_zero() {
            return;
        }
        let quiet = match snapshot.quiet_for {
            Some(quiet) if quiet >= self.stall_after => quiet,
            _ => {
                beat.stalled = None;
                return;
            }
        };
        let subject = snapshot.activity.subject.clone().unwrap_or_default();
        if beat.stalled.as_deref() == Some(subject.as_str()) {
            return;
        }
        beat.stalled = Some(subject.clone());

        let quiet = human(quiet);
        println!(
            "{}: {} has written nothing for {quiet}, leaving it alone",
            self.instance,
            snapshot.doing()
        );
        self.status.note(
            Level::Bad,
            format!("{subject} has written nothing for {quiet}"),
        );
        self.notifier.post_once(
            &stall_key(&subject),
            &format!(
                ":zzz: {} has been running on {subject} for {} without writing a line in {quiet}.",
                self.instance,
                human(snapshot.activity_for)
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::status::{Activity, Stage};

    fn heartbeat(status: Arc<Status>) -> Heartbeat {
        Heartbeat {
            status,
            notifier: Arc::new(Notifier::new(None, "Claudius Maximus".to_string())),
            systemd: Arc::new(Systemd::detached()),
            instance: "Claudius Maximus".to_string(),
            every: Duration::from_secs(60),
            stall_after: Duration::from_secs(1800),
            quiet: false,
        }
    }

    #[test]
    fn a_resting_worker_is_not_news() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::bare(Stage::Resting));
        let heartbeat = heartbeat(status.clone());
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(beat.last_logged, None, "nothing to say between sweeps");
    }

    #[test]
    fn a_run_in_flight_is_logged_once_and_then_only_every_few_ticks() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::run(
            Stage::Implementing,
            "foro-sh/foro#7",
            "claude-sonnet-5",
        ));
        let heartbeat = heartbeat(status.clone());
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        let (first, at) = beat.last_logged.clone().expect("a run in flight is news");
        assert!(first.contains("implementing foro-sh/foro#7"));

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(
            beat.last_logged.as_ref().map(|(_, at)| *at),
            Some(at),
            "the same run doing the same thing does not repeat itself every tick"
        );

        status.doing(Activity::on(Stage::Shipping, "foro-sh/foro#7"));
        // Shipping runs no claude, so the only reason to log is a change, and
        // the rule is that it is not logged at all.
        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(beat.last_logged, None);
    }

    #[test]
    fn a_spent_window_keeps_being_reported_even_though_nothing_is_running() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::bare(Stage::Resting));
        status.window_shut(&crate::limits::Spent::default());
        let heartbeat = heartbeat(status.clone());
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert!(
            beat.last_logged.is_some(),
            "the one stretch where silence used to look like a hang"
        );
    }

    #[test]
    fn a_silent_run_is_said_once_per_run() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        let mut heartbeat = heartbeat(status.clone());
        heartbeat.stall_after = Duration::from_nanos(1);
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(beat.stalled.as_deref(), Some("foro-sh/foro#7"));

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(
            beat.stalled.as_deref(),
            Some("foro-sh/foro#7"),
            "still the same silence, not a new one"
        );

        status.heard("still here");
        heartbeat.stall_after = Duration::from_secs(1800);
        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(beat.stalled, None, "a line ends the silence");
    }

    #[test]
    fn a_quiet_heartbeat_still_beats_but_writes_nothing() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::run(
            Stage::Implementing,
            "foro-sh/foro#7",
            "sonnet",
        ));
        let mut heartbeat = heartbeat(status.clone());
        heartbeat.quiet = true;
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(
            beat.last_logged, None,
            "the journal stays quiet, systemd and the watchdog do not"
        );
    }

    #[test]
    fn silence_can_be_turned_off_entirely() {
        let status = Arc::new(Status::new(&Config::sample()));
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        let mut heartbeat = heartbeat(status.clone());
        heartbeat.stall_after = Duration::ZERO;
        let mut beat = Beat::default();

        heartbeat.tick(&status.snapshot(), &mut beat);
        assert_eq!(beat.stalled, None);
    }
}
