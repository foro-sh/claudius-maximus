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

    /// Posts a line the first time it comes up, and never again.
    ///
    /// Failures repeat: the sweep re-runs every `$POLL_INTERVAL`, and a
    /// backlog that is failing because the subscription's quota is gone fails
    /// on every issue in it. Posting each of those once says the same thing as
    /// posting them a thousand times a day, and stays readable. A restart
    /// clears the memory, which is the right moment to hear it again.
    pub fn post_once(&self, text: &str) {
        if self.said.lock().unwrap().insert(text.to_owned()) {
            self.post(text);
        }
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
