//! `$REPOS` and the rest of the worker's env-var config. Rust port of
//! `infra/claudius-maximus/repos.sh` + `worker.sh`'s config block in the
//! platform repo — same format, same defaults, so migrating an instance's
//! `/etc/claudius-<user>.env` needs no edits.
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
    pub plan_model: String,
    pub plan_effort: String,
    pub implement_model: String,
    pub implement_effort: String,
    pub poll_interval: Duration,
    pub claim_dir: PathBuf,
    pub mattermost_webhook_url: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let repos_raw = std::env::var("REPOS").map_err(|_| {
            anyhow::anyhow!(
                "set REPOS=owner/name=/path/to/clone[,owner/name2=/path/to/clone2]"
            )
        })?;
        let repos = parse_repos(&repos_raw)?;

        Ok(Config {
            repos,
            label: env_or("LABEL", "claudius-maximus"),
            instance: env_or("INSTANCE", "Claudius Maximus"),
            plan_model: env_or("PLAN_MODEL", "claude-opus-5"),
            plan_effort: env_or("PLAN_EFFORT", "high"),
            implement_model: env_or("IMPLEMENT_MODEL", "claude-sonnet-5"),
            implement_effort: env_or("IMPLEMENT_EFFORT", "high"),
            poll_interval: Duration::from_secs(
                env_or("POLL_INTERVAL", "60").parse().map_err(|_| {
                    anyhow::anyhow!("POLL_INTERVAL must be a whole number of seconds")
                })?,
            ),
            claim_dir: PathBuf::from(env_or("CLAUDIUS_CLAIM_DIR", "/tmp")),
            mattermost_webhook_url: std::env::var("CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL").ok(),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_unrestricted_repo() {
        let repos = parse_repos("foro-sh/platform=/home/claudebot/repos/platform").unwrap();
        assert_eq!(
            repos,
            vec![RepoEntry {
                repo: "foro-sh/platform".to_string(),
                clone_path: PathBuf::from("/home/claudebot/repos/platform"),
                authors: vec![],
            }]
        );
    }

    #[test]
    fn parses_multiple_repos_with_an_author_allowlist() {
        let repos = parse_repos(
            "foro-sh/platform=/repos/platform,foro-sh/foro=/repos/foro=alice|bob",
        )
        .unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[1].repo, "foro-sh/foro");
        assert_eq!(repos[1].authors, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn rejects_a_missing_leading_slash() {
        assert!(parse_repos("foro-sh/platform=repos/platform").is_err());
    }

    #[test]
    fn rejects_a_trailing_equals_with_no_authors() {
        assert!(parse_repos("foro-sh/platform=/repos/platform=").is_err());
    }

    #[test]
    fn rejects_an_empty_repos_var() {
        assert!(parse_repos("").is_err());
    }
}
