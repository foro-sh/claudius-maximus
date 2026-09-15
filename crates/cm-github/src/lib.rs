//! The trait `cm-worker` drives GitHub through. Kept separate from any real
//! implementation so the worker's state machine can be built and tested
//! against a fake before the octocrab/device-flow implementation lands
//! (foro-sh/claudius-maximus#1).
use async_trait::async_trait;

/// One issue as the worker's state machine needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub author: String,
}

/// Everything the worker needs from GitHub. `repo` is always `"owner/name"`.
///
/// This is the entire GitHub surface `worker.sh` used through `gh` today:
/// list issues carrying a label, read/add/remove labels, comment, and check
/// the `blocked_by` dependency relationship. A real implementation
/// authenticates via GitHub's OAuth device flow and stores the token with the
/// `keyring` crate — see #1. No implementation lives in this crate; it only
/// defines the contract.
#[async_trait]
pub trait GithubClient: Send + Sync {
    async fn list_labeled_issues(&self, repo: &str, label: &str) -> anyhow::Result<Vec<Issue>>;
    async fn issue_labels(&self, repo: &str, number: u64) -> anyhow::Result<Vec<String>>;
    async fn add_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()>;
    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()>;
    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<()>;
    /// True if any open issue is listed under this issue's `blocked_by`
    /// dependency relationship (the GitHub feature `declaring-issue-dependencies`
    /// in the platform repo writes edges into).
    async fn blocked_by_open_issue(&self, repo: &str, number: u64) -> anyhow::Result<bool>;
}
