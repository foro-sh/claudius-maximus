//! Mattermost status lines. Port of `worker.sh`'s `notify`.
//!
//! A blocking POST is enough: the worker is serial, and a sweep that waits
//! 10 seconds on a webhook is waiting behind a Claude run that takes minutes.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

pub struct Notifier {
    webhook_url: Option<String>,
    instance: String,
    /// Every line already posted through [`Notifier::post_once`].
    said: Mutex<HashSet<String>>,
}

impl Notifier {
    pub fn new(webhook_url: Option<String>, instance: String) -> Self {
        Notifier {
            webhook_url,
            instance,
            said: Mutex::new(HashSet::new()),
        }
    }

    /// Posts a line the first time `key` comes up, and never again.
    ///
    /// Failures repeat: the sweep re-runs every `$POLL_INTERVAL`, and a
    /// backlog failing because the subscription's quota is gone fails on every
    /// issue in it. Posting each of those once says the same thing as posting
    /// them a thousand times a day, and stays readable.
    ///
    /// `key` names what is failing (a repo, or one issue at one stage), and
    /// deliberately not *why*: the commonest error carries a whole `claude`
    /// stderr, which varies run to run, so keying on it would post every sweep
    /// and keep a copy of each message for the life of the process.
    /// [`Notifier::forget`] is what re-arms a key, so each failure is said once
    /// per run of bad luck rather than once ever.
    pub fn post_once(&self, key: &str, text: &str) {
        if self.said.lock().unwrap().contains(key) {
            return;
        }
        // Remembered only once it was actually delivered: a webhook that was
        // down for this one POST must not silence the line for good.
        if self.deliver(text) {
            self.said.lock().unwrap().insert(key.to_owned());
        }
    }

    /// Forgets a key, so a failure that recurs after things worked again is
    /// heard rather than swallowed as old news.
    pub fn forget(&self, key: &str) {
        self.said.lock().unwrap().remove(key);
    }

    /// Posts to Mattermost if configured. Never fails the worker on a bad
    /// post: a dead webhook must not stop the backlog from draining.
    pub fn post(&self, text: &str) {
        self.deliver(text);
    }

    /// Posts, reporting whether Mattermost took it. `false` also covers "there
    /// is no webhook configured", which is nothing to remember either.
    fn deliver(&self, text: &str) -> bool {
        let Some(url) = &self.webhook_url else {
            return false;
        };
        let body = serde_json::json!({
            "username": self.instance,
            "icon_emoji": ":crown:",
            "text": text,
        });
        match ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .post(url)
            .send_json(body)
        {
            Ok(_) => true,
            Err(err) => {
                println!(
                    "{}: mattermost notify failed (ignored): {err}",
                    self.instance
                );
                false
            }
        }
    }
}
