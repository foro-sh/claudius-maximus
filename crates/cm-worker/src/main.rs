mod claim;
mod claude_cli;
mod config;

use claim::ClaimError;
use config::Config;

// Scaffold only. The state machine (plan -> implement -> done), the poll loop
// and Mattermost notify all still need to land here against
// `cm_github::GithubClient` and `cm_git::GitOps` — see
// foro-sh/claudius-maximus#1.
fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;

    // Claim the label before touching GitHub: two workers on one label open
    // competing PRs on the same issue.
    let _claim = match claim::claim_label(&config.claim_dir, &config.label, &config.instance) {
        Ok(claim) => claim,
        Err(ClaimError::Held { holder }) => anyhow::bail!(
            "label {} is already being drained by {holder} — two workers on one label open \
             competing PRs. Give this instance its own LABEL.",
            config.label
        ),
        Err(ClaimError::Fatal(err)) => return Err(err),
    };

    println!("{config:#?}");
    Ok(())
}
