//! In-crate fakes for the worker's three boundaries: GitHub, git, and the
//! `claude` CLI. Same spirit as `worker.sh`'s `tests/stubs/` — fake the
//! boundary, run the real state machine against it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use cm_git::GitOps;
use cm_github::{GithubClient, Issue, IssueComment};

use crate::claude_cli::Claude;

/// One issue as the fake serves it: the labels it carries and whether an open
/// issue blocks it.
#[derive(Debug, Clone)]
pub struct FakeIssue {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub blocked: bool,
}

impl FakeIssue {
    pub fn new(repo: &str, number: u64, author: &str, labels: &[&str]) -> Self {
        FakeIssue {
            repo: repo.to_string(),
            number,
            author: author.to_string(),
            title: format!("issue {number}"),
            body: format!("the work order for issue {number}"),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            comments: Vec::new(),
            blocked: false,
        }
    }

    pub fn blocked(mut self) -> Self {
        self.blocked = true;
        self
    }

    /// An issue that was planned on an earlier sweep, carrying the comment
    /// that sweep left behind.
    pub fn with_comment(self, body: &str) -> Self {
        self.with_comment_by(FakeGithub::LOGIN, body)
    }

    /// A comment somebody other than the worker left.
    pub fn with_comment_by(mut self, author: &str, body: &str) -> Self {
        self.comments.push(IssueComment {
            author: author.to_string(),
            body: body.to_string(),
        });
        self
    }
}

#[derive(Default)]
struct GithubState {
    issues: Vec<FakeIssue>,
    /// Every mutating/reading call, in order — the equivalent of the bash
    /// suite's `$CALLS` file.
    calls: Vec<String>,
    comments: Vec<(String, u64, String)>,
    /// `(repo, head, body)` of every PR opened, so a test can prove exactly
    /// one was, off the right branch.
    pulls: Vec<(String, String, String)>,
}

#[derive(Default)]
pub struct FakeGithub {
    state: Mutex<GithubState>,
}

impl FakeGithub {
    /// The login the fake acts as, i.e. the author of everything the worker
    /// posts through it.
    pub const LOGIN: &'static str = "claudius-bot";

    pub fn new(issues: Vec<FakeIssue>) -> Self {
        FakeGithub {
            state: Mutex::new(GithubState {
                issues,
                ..GithubState::default()
            }),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn comments(&self) -> Vec<(String, u64, String)> {
        self.state.lock().unwrap().comments.clone()
    }

    pub fn pulls(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().pulls.clone()
    }

    /// The labels an issue carries now, after everything the worker did to it.
    pub fn labels(&self, repo: &str, number: u64) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .issues
            .iter()
            .find(|i| i.repo == repo && i.number == number)
            .map(|i| i.labels.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl GithubClient for FakeGithub {
    fn login(&self) -> &str {
        Self::LOGIN
    }

    async fn list_labeled_issues(&self, repo: &str, label: &str) -> anyhow::Result<Vec<Issue>> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(format!("list repo={repo} label={label}"));
        let mut issues: Vec<Issue> = state
            .issues
            .iter()
            .filter(|i| i.repo == repo && i.labels.iter().any(|l| l == label))
            .map(|i| Issue {
                number: i.number,
                author: i.author.clone(),
                title: i.title.clone(),
                body: i.body.clone(),
            })
            .collect();
        issues.sort_by_key(|i| i.number);
        Ok(issues)
    }

    async fn issue_labels(&self, repo: &str, number: u64) -> anyhow::Result<Vec<String>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .issues
            .iter()
            .find(|i| i.repo == repo && i.number == number)
            .map(|i| i.labels.clone())
            .unwrap_or_default())
    }

    async fn add_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(format!("add repo={repo} num={number} label={label}"));
        if let Some(issue) = state
            .issues
            .iter_mut()
            .find(|i| i.repo == repo && i.number == number)
        {
            issue.labels.push(label.to_string());
        }
        Ok(())
    }

    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(format!("remove repo={repo} num={number} label={label}"));
        if let Some(issue) = state
            .issues
            .iter_mut()
            .find(|i| i.repo == repo && i.number == number)
        {
            issue.labels.retain(|l| l != label);
        }
        Ok(())
    }

    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(format!("comment repo={repo} num={number}"));
        state
            .comments
            .push((repo.to_string(), number, body.to_string()));
        if let Some(issue) = state
            .issues
            .iter_mut()
            .find(|i| i.repo == repo && i.number == number)
        {
            issue.comments.push(IssueComment {
                author: Self::LOGIN.to_string(),
                body: body.to_string(),
            });
        }
        Ok(())
    }

    async fn issue_comments(&self, repo: &str, number: u64) -> anyhow::Result<Vec<IssueComment>> {
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(format!("issue_comments repo={repo} num={number}"));
        Ok(state
            .issues
            .iter()
            .find(|i| i.repo == repo && i.number == number)
            .map(|i| i.comments.clone())
            .unwrap_or_default())
    }

    async fn create_pull_request(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(format!(
            "create_pr repo={repo} head={head} base={base} title={title}"
        ));
        let number = state.pulls.len() + 1;
        state
            .pulls
            .push((repo.to_string(), head.to_string(), body.to_string()));
        Ok(format!("https://github.com/{repo}/pull/{number}"))
    }

    async fn blocked_by_open_issue(&self, repo: &str, number: u64) -> anyhow::Result<bool> {
        let mut state = self.state.lock().unwrap();
        state
            .calls
            .push(format!("blocked_by repo={repo} num={number}"));
        Ok(state
            .issues
            .iter()
            .find(|i| i.repo == repo && i.number == number)
            .is_some_and(|i| i.blocked))
    }
}

/// A git that always syncs. `with_default` names origin's default branch,
/// which is what every PR is opened against.
pub struct FakeGit {
    calls: Mutex<Vec<String>>,
    default_branch: String,
}

impl Default for FakeGit {
    fn default() -> Self {
        FakeGit::with_default("main")
    }
}

impl FakeGit {
    pub fn with_default(branch: &str) -> Self {
        FakeGit {
            calls: Mutex::new(Vec::new()),
            default_branch: branch.to_string(),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl GitOps for FakeGit {
    fn sync_default(&self, clone_path: &Path, _token: &str) -> anyhow::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("sync_default path={}", clone_path.display()));
        Ok(self.default_branch.clone())
    }

    fn push(&self, clone_path: &Path, branch: &str, _token: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!(
            "push path={} branch={branch}",
            clone_path.display()
        ));
        Ok(())
    }
}

/// Records what the worker would have asked Claude to do, and answers with
/// canned output — `plan_text` for the plan step, failure for clones whose
/// directory name is in `fail_for` (the bash stub's `$FAIL_CLAUDE_FOR`).
pub struct FakeClaude {
    plan_text: String,
    fail_for: Vec<String>,
    runs: Mutex<Vec<(PathBuf, String)>>,
}

impl Default for FakeClaude {
    fn default() -> Self {
        FakeClaude {
            plan_text: "## Plan\ndo the thing".to_string(),
            fail_for: Vec::new(),
            runs: Mutex::new(Vec::new()),
        }
    }
}

impl FakeClaude {
    pub fn with_plan(plan_text: &str) -> Self {
        FakeClaude {
            plan_text: plan_text.to_string(),
            ..FakeClaude::default()
        }
    }

    pub fn failing_for(clone_dir_names: &[&str]) -> Self {
        FakeClaude {
            fail_for: clone_dir_names.iter().map(|d| d.to_string()).collect(),
            ..FakeClaude::default()
        }
    }

    /// Every `(clone path, prompt)` the worker ran, in order.
    pub fn runs(&self) -> Vec<(PathBuf, String)> {
        self.runs.lock().unwrap().clone()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.runs().into_iter().map(|(_, prompt)| prompt).collect()
    }
}

impl Claude for FakeClaude {
    fn run(
        &self,
        repo_dir: &Path,
        _model: &str,
        _effort: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        self.runs
            .lock()
            .unwrap()
            .push((repo_dir.to_path_buf(), prompt.to_string()));
        let dir_name = repo_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.fail_for.contains(&dir_name) {
            anyhow::bail!("claude failed for {dir_name}");
        }
        Ok(self.plan_text.clone())
    }
}
