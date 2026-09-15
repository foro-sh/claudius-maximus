//! In-crate fakes for the worker's three boundaries: GitHub, git, and the
//! `claude` CLI. Same spirit as `worker.sh`'s `tests/stubs/` — fake the
//! boundary, run the real state machine against it.
//!
//! They live outside `#[cfg(test)]` because `main` currently wires them in
//! too: the octocrab and git2 implementations of these traits are landing
//! separately (foro-sh/claudius-maximus#1).

#![allow(dead_code)] // the binary wires in a subset; the rest is for the tests

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use cm_git::GitOps;
use cm_github::{GithubClient, Issue};

use crate::claude_cli::Claude;

/// One issue as the fake serves it: the labels it carries and whether an open
/// issue blocks it.
#[derive(Debug, Clone)]
pub struct FakeIssue {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub labels: Vec<String>,
    pub blocked: bool,
}

impl FakeIssue {
    pub fn new(repo: &str, number: u64, author: &str, labels: &[&str]) -> Self {
        FakeIssue {
            repo: repo.to_string(),
            number,
            author: author.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            blocked: false,
        }
    }

    pub fn blocked(mut self) -> Self {
        self.blocked = true;
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
}

#[derive(Default)]
pub struct FakeGithub {
    state: Mutex<GithubState>,
}

impl FakeGithub {
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
        Ok(())
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

#[derive(Default)]
pub struct FakeGit {
    calls: Mutex<Vec<String>>,
}

impl FakeGit {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl GitOps for FakeGit {
    fn sync_branch(&self, clone_path: &Path, branch: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!(
            "sync_branch path={} branch={branch}",
            clone_path.display()
        ));
        Ok(())
    }

    fn commit(
        &self,
        clone_path: &Path,
        message: &str,
        _author_name: &str,
        _author_email: &str,
    ) -> anyhow::Result<Option<String>> {
        self.calls.lock().unwrap().push(format!(
            "commit path={} message={message}",
            clone_path.display()
        ));
        Ok(None)
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
