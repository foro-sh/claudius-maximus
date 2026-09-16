//! The trait `cm-worker` drives GitHub through, plus its `octocrab`
//! implementation. Kept as a trait so the worker's state machine can also be
//! tested against a fake (foro-sh/claudius-maximus#1).
use async_trait::async_trait;

mod client;
mod token_store;

pub use client::OctocrabGithubClient;

/// One issue as the worker's state machine needs to see it. The title rides
/// along with the list, so the PR can be titled after the issue without a
/// second call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub author: String,
    pub title: String,
}

/// Everything the worker needs from GitHub. `repo` is always `"owner/name"`.
///
/// This is the entire GitHub surface `worker.sh` used through `gh` today:
/// list issues carrying a label, read/add/remove labels, comment, and check
/// the `blocked_by` dependency relationship. A real implementation
/// authenticates via GitHub's OAuth device flow and stores the token with the
/// `keyring` crate — see #1, and [`OctocrabGithubClient`].
#[async_trait]
pub trait GithubClient: Send + Sync {
    /// Open issues carrying `label`, pull requests excluded. A closed issue
    /// is finished work no matter what labels it still has on it.
    async fn list_labeled_issues(&self, repo: &str, label: &str) -> anyhow::Result<Vec<Issue>>;
    async fn issue_labels(&self, repo: &str, number: u64) -> anyhow::Result<Vec<String>>;
    async fn add_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()>;
    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()>;
    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<()>;
    /// Open a pull request from `head` onto `base`, returning its URL. If one
    /// is already open for `head` — the branch is reused across retries — its
    /// URL comes back instead: a second PR for one issue is the thing to
    /// avoid, not an error to report.
    async fn create_pull_request(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<String>;

    /// True if any open issue is listed under this issue's `blocked_by`
    /// dependency relationship (the GitHub feature `declaring-issue-dependencies`
    /// in the platform repo writes edges into).
    async fn blocked_by_open_issue(&self, repo: &str, number: u64) -> anyhow::Result<bool>;
}
