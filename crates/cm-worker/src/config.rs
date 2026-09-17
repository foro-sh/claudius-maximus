//! `$REPOS` and the rest of the worker's env-var config. Rust port of the
//! bash worker's `repos.sh` + config block, same format, same defaults, so
//! migrating an instance's `/etc/<user>.env` needs no edits.
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// `"owner/name"`.
    pub repo: String,
    pub clone_path: PathBuf,
    /// Empty means every issue author is trusted (only safe for a repo where
    /// filing an issue already requires access).
    pub authors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub repos: Vec<RepoEntry>,
    pub label: String,
    pub instance: String,
    /// The GitHub OAuth App the device flow authorizes against (a deployment
    /// detail, so it comes from the env like everything else).
    pub github_client_id: String,
    pub plan_model: String,
    pub plan_effort: String,
    pub implement_model: String,
    pub implement_effort: String,
    pub poll_interval: Duration,
    /// How often the heartbeat looks at what the worker is doing. Zero turns
    /// the thread off entirely, for anyone who wants the old silence back.
    pub heartbeat_interval: Duration,
    /// How long a `claude` run may write nothing before the worker says so.
    /// Zero turns that off. It is only ever said, never acted on: a run that
    /// has been thinking for an hour is still an hour of work worth having.
    pub stall_after: Duration,
    pub claim_dir: PathBuf,
    pub mattermost_webhook_url: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let repos_raw = std::env::var("REPOS").map_err(|_| {
            anyhow::anyhow!("set REPOS=owner/name=/path/to/clone[,owner/name2=/path/to/clone2]")
        })?;
        let repos = parse_repos(&repos_raw)?;
        let github_client_id = std::env::var("GITHUB_CLIENT_ID").map_err(|_| {
            anyhow::anyhow!(
                "set GITHUB_CLIENT_ID to the client id of a GitHub OAuth App with device flow enabled; the worker authorizes against it"
            )
        })?;

        Ok(Config {
            repos,
            github_client_id,
            label: env_or("LABEL", "claudius-maximus"),
            instance: env_or("INSTANCE", "Claudius Maximus"),
            plan_model: env_or("PLAN_MODEL", "claude-opus-5"),
            plan_effort: env_or("PLAN_EFFORT", "high"),
            implement_model: env_or("IMPLEMENT_MODEL", "claude-sonnet-5"),
            implement_effort: env_or("IMPLEMENT_EFFORT", "high"),
            poll_interval: env_secs("POLL_INTERVAL", 60)?,
            heartbeat_interval: env_secs("HEARTBEAT_INTERVAL", 60)?,
            stall_after: env_secs("STALL_AFTER", 1800)?,
            claim_dir: PathBuf::from(env_or("CLAUDIUS_CLAIM_DIR", "/tmp")),
            mattermost_webhook_url: std::env::var("CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL").ok(),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// A whole number of seconds out of the environment. A typo here is fatal
/// rather than defaulted: `HEARTBEAT_INTERVAL=1m` silently meaning 60 seconds
/// on one box and 0 on another is exactly the kind of thing nobody notices
/// until they are reading a journal that has been quiet for a week.
fn env_secs(key: &str, default: u64) -> anyhow::Result<Duration> {
    let Ok(raw) = std::env::var(key) else {
        return Ok(Duration::from_secs(default));
    };
    let secs: u64 = raw
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{key} must be a whole number of seconds, got '{raw}'"))?;
    Ok(Duration::from_secs(secs))
}

/// Parses the same format as `repos.sh`'s `parse_repos`: comma-separated
/// `owner/name=/abs/path/to/clone[=author|author]` entries, no spaces. A
/// malformed entry is an error rather than a silently-wrong tree or a
/// silently-widened allowlist.
fn parse_repos(raw: &str) -> anyhow::Result<Vec<RepoEntry>> {
    let mut repos = Vec::new();
    for entry in raw.split(',') {
        if entry.is_empty() {
            continue;
        }
        let (repo, rest) = entry.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid REPOS entry: '{entry}' (want owner/name=/abs/path/to/clone[=author|author])"
            )
        })?;
        if !repo.contains('/') || !rest.starts_with('/') {
            anyhow::bail!(
                "invalid REPOS entry: '{entry}' (want owner/name=/abs/path/to/clone[=author|author])"
            );
        }
        let (clone_path, authors) = match rest.split_once('=') {
            Some((dir, authors)) => {
                if authors.is_empty() {
                    anyhow::bail!(
                        "invalid REPOS entry: '{entry}' (trailing '=' with no author allowlist)"
                    );
                }
                (dir, authors.split('|').map(str::to_string).collect())
            }
            None => (rest, Vec::new()),
        };
        repos.push(RepoEntry {
            repo: repo.to_string(),
            clone_path: PathBuf::from(clone_path),
            authors,
        });
    }
    if repos.is_empty() {
        anyhow::bail!("REPOS is empty (want owner/name=/abs/path/to/clone[,...])");
    }
    Ok(repos)
}

#[cfg(test)]
impl Config {
    /// A config to build test cases on, so that a new knob is one edit here
    /// rather than one in every module that needs a `Config` to test with.
    pub fn sample() -> Config {
        Config {
            repos: vec![RepoEntry {
                repo: "foro-sh/foro".to_string(),
                clone_path: PathBuf::from("/clones/foro"),
                authors: vec![],
            }],
            github_client_id: "Iv1.testclientid".to_string(),
            label: "claudius-maximus".to_string(),
            instance: "Claudius Maximus".to_string(),
            plan_model: "claude-opus-5".to_string(),
            plan_effort: "high".to_string(),
            implement_model: "claude-sonnet-5".to_string(),
            implement_effort: "high".to_string(),
            poll_interval: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(60),
            stall_after: Duration::from_secs(1800),
            claim_dir: PathBuf::from("/tmp"),
            mattermost_webhook_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_unrestricted_repo() {
        let repos =
            parse_repos("foro-sh/claudius-maximus=/home/claudius-maximus/repos/claudius-maximus")
                .unwrap();
        assert_eq!(
            repos,
            vec![RepoEntry {
                repo: "foro-sh/claudius-maximus".to_string(),
                clone_path: PathBuf::from("/home/claudius-maximus/repos/claudius-maximus"),
                authors: vec![],
            }]
        );
    }

    #[test]
    fn parses_multiple_repos_with_an_author_allowlist() {
        let repos = parse_repos(
            "foro-sh/claudius-maximus=/repos/claudius-maximus,foro-sh/foro=/repos/foro=alice|bob",
        )
        .unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[1].repo, "foro-sh/foro");
        assert_eq!(
            repos[1].authors,
            vec!["alice".to_string(), "bob".to_string()]
        );
    }

    #[test]
    fn rejects_a_missing_leading_slash() {
        assert!(parse_repos("foro-sh/claudius-maximus=repos/claudius-maximus").is_err());
    }

    #[test]
    fn rejects_a_trailing_equals_with_no_authors() {
        assert!(parse_repos("foro-sh/claudius-maximus=/repos/claudius-maximus=").is_err());
    }

    #[test]
    fn rejects_an_empty_repos_var() {
        assert!(parse_repos("").is_err());
    }
}
