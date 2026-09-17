mod backoff;
mod claim;
mod claude_cli;
mod config;
#[cfg(test)]
mod fakes;
mod heartbeat;
mod httpd;
mod limits;
mod notify;
mod status;
mod systemd;
mod worker;

use std::sync::Arc;

use backoff::Backoff;
use claim::ClaimError;
use cm_github::OctocrabGithubClient;
use config::Config;
use heartbeat::Heartbeat;
use notify::Notifier;
use status::{Activity, Stage, Status};
use systemd::Systemd;
use worker::Worker;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First, and before any thread exists: reading this takes systemd's
    // variables back out of the environment, which is only sound while this is
    // the only thread running.
    let systemd = Arc::new(Systemd::from_env());

    let config = Config::from_env()?;
    let status = Arc::new(Status::new(&config));
    let notifier = Arc::new(Notifier::new(
        config.mattermost_webhook_url.clone(),
        config.instance.clone(),
    ));

    // Claim the label before touching GitHub: two workers on one label open
    // competing PRs on the same issue.
    let _claim = match claim::claim_label(&config.claim_dir, &config.label, &config.instance) {
        Ok(claim) => claim,
        Err(ClaimError::Held { holder }) => {
            notifier.post(&format!(
                ":no_entry: {} refused to start: `{}` is already claimed by {holder}.",
                config.instance, config.label
            ));
            anyhow::bail!(
                "label {} is already being drained by {holder}. Two workers on one label open \
                 competing PRs. Give this instance its own LABEL.",
                config.label
            );
        }
        Err(ClaimError::Fatal(err)) => return Err(err),
    };

    // Told before the device flow, not after: that flow waits on a human
    // opening a URL, and systemd would count the wait against
    // `TimeoutStartSec` and kill the unit somebody is in the middle of
    // authorizing.
    systemd.ready();
    if systemd.supervised() {
        println!(
            "{}: systemd is listening; status lines go to `systemctl status`{}",
            config.instance,
            match systemd.watchdog_interval() {
                Some(every) => format!(", watchdog every {}s", every.as_secs()),
                None => String::new(),
            }
        );
    }
    // `HEARTBEAT_INTERVAL=0` asks for no journal lines, which is not the same
    // as asking for no thread: under a unit with a watchdog, something has to
    // answer it or systemd restarts a perfectly healthy worker every few
    // minutes.
    let beat_every = match (config.heartbeat_interval, systemd.watchdog_interval()) {
        (interval, _) if !interval.is_zero() => Some(interval),
        (_, Some(watchdog)) => Some(watchdog / 2),
        _ => None,
    };
    // Up before the device flow rather than after it, so that the page can
    // answer "what is it waiting for" during the one stage that waits on a
    // human. A port already in use is fatal here: an instance nobody can watch
    // is exactly the thing this is for.
    if let Some(addr) = config.status_addr {
        httpd::serve(addr, status.clone())?;
        println!("{}: status page on http://{addr}/", config.instance);
    }

    if let Some(every) = beat_every {
        Heartbeat {
            status: status.clone(),
            notifier: notifier.clone(),
            systemd: systemd.clone(),
            instance: config.instance.clone(),
            every,
            stall_after: config.stall_after,
            quiet: config.heartbeat_interval.is_zero(),
        }
        .start();
    }

    // Blocks on the device flow the first time this instance runs, so the
    // one-time authorization happens before the first sweep rather than in the
    // middle of one. It is also the one stage that waits on a human, so it
    // says so: an instance stuck here is stuck on somebody opening a URL.
    status.doing(Activity::bare(Stage::Authorizing));
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
        status: &status,
    }
    .run()
    .await
}
