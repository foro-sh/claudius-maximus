mod claim;
mod claude_cli;
mod config;
mod notify;

use claim::ClaimError;
use config::Config;
use notify::Notifier;

// Scaffold only. The state machine (plan -> implement -> done), the poll loop
// and Mattermost notify all still need to land here against
// `cm_github::GithubClient` and `cm_git::GitOps` — see
// foro-sh/claudius-maximus#1.
fn main() -> anyhow::Result<()> {
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

    println!("{config:#?}");
    Ok(())
}
