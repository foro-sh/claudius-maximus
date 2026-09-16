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
    /// `key` is separate from the text so it can carry the error the text
    /// does not: one repo-wide outage collapses to one line however many
    /// issues hit it, while the same issue failing later for a different
    /// reason is a different key, and gets said.
    pub fn post_once(&self, key: &str, text: &str) {
        let mut said = self.said.lock().unwrap();
        // A failure whose message varies every sweep — a stderr dump, an error
        // carrying a request id — would otherwise grow this set forever in a
        // process built to run for weeks. Forgetting everything at the cap
        // costs at most one repeated line per failure still outstanding.
        if said.len() >= 512 {
            said.clear();
        }
        if said.insert(key.to_owned()) {
            drop(said);
            self.post(text);
        }
    }

    /// Forgets every key starting with `prefix`, so a failure that recurs
    /// after things worked again is heard rather than swallowed as old news.
    pub fn forget(&self, prefix: &str) {
        self.said
            .lock()
            .unwrap()
            .retain(|key| !key.starts_with(prefix));
    }

    /// Posts to Mattermost if configured. Never fails the worker on a bad
    /// post — a dead webhook must not stop the backlog from draining.
    pub fn post(&self, text: &str) {
        let Some(url) = &self.webhook_url else {
            return;
        };
        let body = serde_json::json!({
            "username": self.instance,
            "icon_emoji": ":crown:",
            "text": text,
        });
        if let Err(err) = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .post(url)
            .send_json(body)
        {
            println!(
                "{}: mattermost notify failed (ignored): {err}",
                self.instance
            );
        }
    }
}
