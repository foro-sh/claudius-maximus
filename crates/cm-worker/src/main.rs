mod claim;
mod claude_cli;
mod config;
mod fakes;
mod notify;
mod worker;

use claim::ClaimError;
use config::Config;
use notify::Notifier;
use worker::Worker;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let notifier = Notifier::new(
        config.mattermost_webhook_url.clone(),
        config.instance.clone(),
    );

    // Claim the label before touching GitHub: two workers on one label open
    // competing PRs on the same issue.
    let _claim = match claim::claim_label(&config.claim_dir, &config.label, &config.instance) {
        Ok(claim) => claim,
        Err(ClaimError::Held { holder }) => {
            notifier.post(&format!(
                ":no_entry: {} refused to start — `{}` is already claimed by {holder}.",
                config.instance, config.label
            ));
            anyhow::bail!(
                "label {} is already being drained by {holder} — two workers on one label open \
                 competing PRs. Give this instance its own LABEL.",
                config.label
            );
        }
        Err(ClaimError::Fatal(err)) => return Err(err),
    };

    // TODO: swap for OctocrabGithubClient once merged (#4) — the fake keeps the
    // binary runnable until then, and the state machine already talks to the
    // trait, so it is this one line that changes.
    let github = fakes::FakeGithub::default();
    let git = cm_git::Git2Ops;
    let claude = claude_cli::ClaudeCli;

    Worker {
        config: &config,
        github: &github,
        git: &git,
        claude: &claude,
        notifier: &notifier,
    }
    .run()
    .await
}
