mod claude_cli;
mod config;

use config::Config;

// Scaffold only. The state machine (plan -> implement -> review -> done),
// the poll loop, the single-instance-per-label claim, and Mattermost notify
// all still need to land here against `cm_github::GithubClient` and
// `cm_git::GitOps` — see foro-sh/claudius-maximus#1.
fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    println!("{config:#?}");
    Ok(())
}
