//! The trait `cm-worker` drives git through. Kept separate from any real
//! implementation so the worker's state machine can be built and tested
//! against a fake before the `git2` implementation lands
//! (foro-sh/claudius-maximus#1).
use std::path::Path;

/// Everything the worker needs from git — no `git` CLI, per #1.
///
/// A real implementation uses `git2` (vendored libgit2). Because `git2` never
/// invokes repo hooks, `commit` below must itself reject a message that
/// isn't a valid Conventional Commit — there is no `commit-msg` hook backing
/// it up the way there is for a human `git commit`.
pub trait GitOps: Send + Sync {
    /// Fetch `origin` and hard-reset `branch` to `origin/<default branch>`,
    /// creating the local branch if it doesn't exist yet. Equivalent of
    /// `worker.sh`'s `git fetch && git checkout <branch> && git reset --hard`.
    fn sync_branch(&self, clone_path: &Path, branch: &str) -> anyhow::Result<()>;

    /// Commit all pending changes in `clone_path`. Must validate `message`
    /// against Conventional Commits itself before creating the commit —
    /// return `Err` on an invalid message rather than let a non-conforming
    /// commit through, since no hook will catch it later. `Ok(None)` means
    /// there was nothing to commit.
    fn commit(
        &self,
        clone_path: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> anyhow::Result<Option<String>>;

    /// Push `branch` to `origin` over HTTPS, authenticated with `token`
    /// (the same OAuth token `cm-github`'s client holds).
    fn push(&self, clone_path: &Path, branch: &str, token: &str) -> anyhow::Result<()>;
}
