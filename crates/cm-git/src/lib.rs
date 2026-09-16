//! The trait `cm-worker` drives git through, plus its `git2` implementation.
//! Kept as a trait so the worker's state machine can also be tested against a
//! fake (foro-sh/claudius-maximus#1).
use std::path::Path;

use anyhow::{Context, Result};
use git2::build::CheckoutBuilder;
use git2::{
    BranchType, Cred, Direction, FetchOptions, PushOptions, RemoteCallbacks, Repository, ResetType,
};

/// Everything the worker needs from git — no `git` CLI, per #1. Claude makes
/// the commits (with its own git, inside the clone); the worker only syncs the
/// clone and pushes the finished branch, which is the half that needs the
/// instance's OAuth token.
pub trait GitOps: Send + Sync {
    /// Fetch `origin` and hard-reset `branch` to `origin/<default branch>`,
    /// creating the local branch if it doesn't exist yet. Equivalent of
    /// `worker.sh`'s `git fetch && git checkout <branch> && git reset --hard`.
    fn sync_branch(&self, clone_path: &Path, branch: &str) -> anyhow::Result<()>;

    /// Push `branch` to `origin` over HTTPS, authenticated with `token`
    /// (the same OAuth token `cm-github`'s client holds).
    fn push(&self, clone_path: &Path, branch: &str, token: &str) -> anyhow::Result<()>;
}

/// `GitOps` on top of vendored libgit2. `clone_path` is always an existing
/// checked-out clone with an `origin` remote — the initial clone happens
/// elsewhere.
pub struct Git2Ops;

impl GitOps for Git2Ops {
    fn sync_branch(&self, clone_path: &Path, branch: &str) -> Result<()> {
        let repo = Repository::open(clone_path)?;
        let mut remote = repo.find_remote("origin")?;

        // `default_branch` only answers once the remote is connected, so
        // connect explicitly rather than letting `fetch` do it implicitly.
        // ponytail: anonymous fetch — private clones will need the token
        // threaded through `sync_branch` too once the worker touches one.
        remote
            .connect(Direction::Fetch)
            .context("connecting to origin")?;
        let default = remote.default_branch().context("reading origin HEAD")?;
        let default = default
            .as_str()
            .context("origin's default branch is not utf-8")?
            .strip_prefix("refs/heads/")
            .context("origin's default branch is not a branch ref")?
            .to_owned();
        remote.fetch(&[&default], Some(&mut FetchOptions::new()), None)?;
        remote.disconnect()?;

        let upstream = repo
            .find_branch(&format!("origin/{default}"), BranchType::Remote)?
            .into_reference()
            .peel_to_commit()?;

        // A forced reference update rather than `Repository::branch`, which
        // refuses to force a branch that is already HEAD — the common case
        // here, since the worker re-syncs a branch it is already sitting on.
        repo.reference(
            &format!("refs/heads/{branch}"),
            upstream.id(),
            true,
            "sync_branch",
        )?;
        repo.set_head(&format!("refs/heads/{branch}"))?;
        // The checkout is what clears untracked files, which a plain
        // `reset --hard` leaves behind: the worker reuses one clone across
        // issues, so whatever a killed Claude run dropped in the tree would
        // otherwise be swept into the next issue's commits by Claude.
        // Ignored files (`target/`) stay — that's the build cache.
        // `reset` can't do it in one step; it overrides the checkout strategy
        // it is handed.
        let mut checkout = CheckoutBuilder::new();
        checkout.force().remove_untracked(true);
        repo.checkout_tree(upstream.as_object(), Some(&mut checkout))?;
        repo.reset(upstream.as_object(), ResetType::Hard, None)?;
        Ok(())
    }

    fn push(&self, clone_path: &Path, branch: &str, token: &str) -> Result<()> {
        let repo = Repository::open(clone_path)?;
        let mut remote = repo.find_remote("origin")?;

        let mut callbacks = RemoteCallbacks::new();
        // GitHub takes an OAuth token as the password behind any non-empty
        // username.
        callbacks.credentials(|_, _, _| Cred::userpass_plaintext("x-access-token", token));
        let mut options = PushOptions::new();
        options.remote_callbacks(callbacks);

        remote.push(
            &[format!("refs/heads/{branch}:refs/heads/{branch}")],
            Some(&mut options),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use git2::{IndexAddOption, RepositoryInitOptions, Signature};
    use tempfile::TempDir;

    fn commit_all(repo: &Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let who = Signature::now("Fixture", "fixture@example.com").unwrap();
        let parents = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(head) => vec![head],
            None => vec![],
        };
        let parents: Vec<&_> = parents.iter().collect();
        repo.commit(Some("HEAD"), &who, &who, message, &tree, &parents)
            .unwrap();
    }

    /// A bare "remote" whose default branch is `default`, plus a clone of it
    /// holding one commit. Everything is local, so no network and no creds.
    fn fixture(default: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("remote.git");
        let mut options = RepositoryInitOptions::new();
        options.bare(true).initial_head(default);
        Repository::init_opts(&bare, &options).unwrap();

        let clone = tmp.path().join("clone");
        let mut options = RepositoryInitOptions::new();
        options.initial_head(default);
        let repo = Repository::init_opts(&clone, &options).unwrap();
        fs::write(clone.join("README.md"), "seed\n").unwrap();
        commit_all(&repo, "chore: seed");

        let mut remote = repo.remote("origin", bare.to_str().unwrap()).unwrap();
        remote
            .push(
                &[format!("refs/heads/{default}:refs/heads/{default}")],
                None,
            )
            .unwrap();
        drop(remote);
        drop(repo);
        (tmp, clone)
    }

    #[test]
    fn sync_branch_creates_branch_off_the_detected_default() {
        // Deliberately not `main`: the default branch has to be detected.
        let (_tmp, clone) = fixture("trunk");
        Git2Ops.sync_branch(&clone, "cm/issue-1").unwrap();

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("cm/issue-1"));
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            repo.find_branch("origin/trunk", BranchType::Remote)
                .unwrap()
                .into_reference()
                .peel_to_commit()
                .unwrap()
                .id()
        );
    }

    #[test]
    fn sync_branch_reuses_the_branch_and_throws_away_local_work() {
        let (_tmp, clone) = fixture("main");
        Git2Ops.sync_branch(&clone, "cm/issue-2").unwrap();

        fs::write(clone.join("README.md"), "scribbled over\n").unwrap();
        fs::write(clone.join("junk.txt"), "tracked junk\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "chore: local work");
        fs::create_dir_all(clone.join("leftovers")).unwrap();
        fs::write(clone.join("leftovers/scratch.txt"), "never staged\n").unwrap();

        Git2Ops.sync_branch(&clone, "cm/issue-2").unwrap();

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("cm/issue-2"));
        assert_eq!(
            fs::read_to_string(clone.join("README.md")).unwrap(),
            "seed\n"
        );
        assert!(!clone.join("junk.txt").exists());
        assert!(!clone.join("leftovers/scratch.txt").exists());
    }

    /// Needs a real remote and a real token; run with
    /// `CM_GIT_PUSH_REMOTE=https://github.com/<owner>/<repo>.git \
    ///  CM_GIT_PUSH_TOKEN=<token> cargo test -p cm-git -- --ignored`.
    #[test]
    #[ignore = "needs a real remote and a real credential"]
    fn push_sends_the_branch_to_a_real_remote() {
        let url = std::env::var("CM_GIT_PUSH_REMOTE").expect("CM_GIT_PUSH_REMOTE");
        let token = std::env::var("CM_GIT_PUSH_TOKEN").expect("CM_GIT_PUSH_TOKEN");

        let tmp = TempDir::new().unwrap();
        let clone = tmp.path().join("clone");
        let repo = Repository::clone(&url, &clone).unwrap();
        drop(repo);

        let branch = format!("cm/push-test-{}", std::process::id());
        Git2Ops.sync_branch(&clone, &branch).unwrap();
        fs::write(clone.join("push-test.txt"), "hello\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "test: push smoke test");
        Git2Ops.push(&clone, &branch, &token).unwrap();
    }
}
