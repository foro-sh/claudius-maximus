//! Per-issue backoff, so one issue that keeps failing stops eating the sweep.
//!
//! A failed issue keeps its trigger label and is retried on the next sweep —
//! that is the design, and for a transient failure it is the right one. But the
//! retry is a whole Claude run, so an issue that fails *every* time spends
//! minutes of a shared subscription quota on the same failure before the sweep
//! ever reaches the issue behind it. Doubling the wait leaves a transient
//! failure quick to recover from and steps a stuck one out of the way.
//!
//! In memory, like nothing else in this worker: GitHub holds the state that
//! matters, and a restart that retries everything once is exactly what an
//! operator restarting a stuck worker is asking for.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Doublings before the wait stops growing — 2^6 sweeps, so just over an hour
/// at the default one-minute interval. Long enough that a stuck issue costs
/// nothing, short enough that a fixed one is picked up while someone is still
/// watching for it.
const MAX_DOUBLINGS: u32 = 6;

#[derive(Default)]
pub struct Backoff {
    resting: Mutex<HashMap<String, Rest>>,
}

struct Rest {
    failures: u32,
    until: Instant,
}

impl Backoff {
    /// True while `key` is still resting off an earlier failure.
    pub fn resting(&self, key: &str) -> bool {
        self.resting
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|rest| Instant::now() < rest.until)
    }

    /// Records a failure and returns how long `key` now rests for, which is
    /// what the log line says.
    pub fn record_failure(&self, key: &str, poll_interval: Duration) -> Duration {
        let mut resting = self.resting.lock().unwrap();
        let failures = resting.get(key).map_or(0, |rest| rest.failures) + 1;
        let wait = poll_interval * 2u32.pow((failures - 1).min(MAX_DOUBLINGS));
        resting.insert(
            key.to_owned(),
            Rest {
                failures,
                until: Instant::now() + wait,
            },
        );
        wait
    }

    /// Forgets a key, so the next failure there starts over at one interval
    /// rather than at whatever an old run of bad luck had climbed to.
    pub fn forget(&self, key: &str) {
        self.resting.lock().unwrap().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(60);

    #[test]
    fn a_key_that_never_failed_is_not_resting() {
        assert!(!Backoff::default().resting("issue foo/bar#7"));
    }

    #[test]
    fn the_wait_doubles_per_failure_and_then_stops_growing() {
        let backoff = Backoff::default();
        let waits: Vec<Duration> = (0..9)
            .map(|_| backoff.record_failure("issue foo/bar#7", INTERVAL))
            .collect();
        assert_eq!(
            waits,
            vec![
                INTERVAL,
                INTERVAL * 2,
                INTERVAL * 4,
                INTERVAL * 8,
                INTERVAL * 16,
                INTERVAL * 32,
                INTERVAL * 64,
                INTERVAL * 64,
                INTERVAL * 64,
            ]
        );
        assert!(backoff.resting("issue foo/bar#7"));
    }

    #[test]
    fn a_forgotten_key_starts_over() {
        let backoff = Backoff::default();
        backoff.record_failure("issue foo/bar#7", INTERVAL);
        backoff.record_failure("issue foo/bar#7", INTERVAL);
        backoff.forget("issue foo/bar#7");

        assert!(!backoff.resting("issue foo/bar#7"));
        assert_eq!(
            backoff.record_failure("issue foo/bar#7", INTERVAL),
            INTERVAL
        );
    }

    #[test]
    fn one_key_resting_leaves_the_others_alone() {
        let backoff = Backoff::default();
        backoff.record_failure("issue foo/bar#7", INTERVAL);

        assert!(backoff.resting("issue foo/bar#7"));
        assert!(!backoff.resting("issue foo/bar#8"));
    }
}
