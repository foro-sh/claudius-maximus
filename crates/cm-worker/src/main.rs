mod backoff;
mod claim;
mod claude_cli;
mod config;
#[cfg(test)]
mod fakes;
mod notify;
mod worker;

use backoff::Backoff;
use claim::ClaimError;
use cm_github::OctocrabGithubClient;
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

    // Blocks on the device flow the first time this instance runs, so the
    // one-time authorization happens before the first sweep rather than in the
    // middle of one.
    let github =
        OctocrabGithubClient::login_or_load(&config.instance, &config.github_client_id).await?;
    let token = github.token().to_owned();
    let git = cm_git::Git2Ops;
    let claude = claude_cli::ClaudeCli;

    Worker {
        config: &config,
        github: &github,
        git: &git,
        claude: &claude,
        notifier: &notifier,
        backoff: &Backoff::default(),
        token: &token,
    }
    .run()
    .await
}
