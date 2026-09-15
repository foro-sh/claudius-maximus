//! Mattermost status lines. Port of `worker.sh`'s `notify`.
//!
//! A blocking POST is enough: the worker is serial, and a sweep that waits
//! 10 seconds on a webhook is waiting behind a Claude run that takes minutes.

use std::time::Duration;

pub struct Notifier {
    webhook_url: Option<String>,
    instance: String,
}

impl Notifier {
    pub fn new(webhook_url: Option<String>, instance: String) -> Self {
        Notifier {
            webhook_url,
            instance,
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
