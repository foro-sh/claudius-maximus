//! The trait `cm-worker` drives git through, plus its `git2` implementation.
//! Kept as a trait so the worker's state machine can also be tested against a
//! fake (foro-sh/claudius-maximus#1).
use std::path::Path;

use anyhow::{Context, Result};
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{
    BranchType, Cred, Direction, ErrorCode, FetchOptions, PushOptions, RemoteCallbacks, Repository,
    ResetType,
};

/// Everything the worker needs from git (no `git` CLI, per #1). Claude makes
/// the commits (with its own git, inside the clone); the worker only syncs the
/// clone and pushes the finished branch, which is the half that needs the
/// instance's OAuth token.
pub trait GitOps: Send + Sync {
    /// Fetch `origin`, hard-reset the local copy of origin's default branch
    /// onto it, check it out, and return that branch's name. Equivalent of
    /// `worker.sh`'s `git fetch && git checkout <default> && git reset --hard`.
    /// Authenticated with `token`, like [`GitOps::push`]: the repos the worker
    /// serves are private, so an anonymous fetch never sees them.
    ///
    /// Clones `remote_url` first if there is nothing at `clone_path` yet. The
    /// worker's own token is the only credential on the box that reaches a
    /// private repo, so the first clone belongs here rather than in whatever
    /// provisioned the instance.
    ///
    /// The branch name comes back rather than going in: it is also the base
    /// every PR is opened against, and a repo whose default is not `main`
    /// would otherwise get a PR aimed at a branch that does not exist.
    fn sync_default(
        &self,
        clone_path: &Path,
        remote_url: &str,
        token: &str,
    ) -> anyhow::Result<String>;

    /// True if `branch` exists locally and carries commits `base` does not.
    /// False for a branch that was never created: a Claude run that committed
    /// nothing leaves one or the other, and neither is a pull request.
    fn has_new_commits(&self, clone_path: &Path, branch: &str, base: &str) -> anyhow::Result<bool>;

    /// Push `branch` to `origin` over HTTPS, authenticated with `token`
    /// (the same OAuth token `cm-github`'s client holds). A ref origin
    /// refuses is an error, not a push that quietly did nothing.
    fn push(&self, clone_path: &Path, branch: &str, token: &str) -> anyhow::Result<()>;
}

/// `GitOps` on top of vendored libgit2.
pub struct Git2Ops;

impl GitOps for Git2Ops {
    fn sync_default(&self, clone_path: &Path, remote_url: &str, token: &str) -> Result<String> {
        let repo = open_or_clone(clone_path, remote_url, token)?;
        let mut remote = repo.find_remote("origin")?;

        // `default_branch` only answers once the remote is connected, so
        // connect explicitly rather than letting `fetch` do it implicitly. The
        // connection is dropped before the fetch, which opens its own.
        let default = {
            let connection = remote
                .connect_auth(Direction::Fetch, Some(credentials(token)), None)
                .context("connecting to origin")?;
            let default = connection.default_branch().context("reading origin HEAD")?;
            default
                .as_str()
                .context("origin's default branch is not utf-8")?
                .strip_prefix("refs/heads/")
                .context("origin's default branch is not a branch ref")?
                .to_owned()
        };

        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(credentials(token));
        remote.fetch(&[&default], Some(&mut fetch_options), None)?;

        let upstream = repo
            .find_branch(&format!("origin/{default}"), BranchType::Remote)?
            .into_reference()
            .peel_to_commit()?;

        // A forced reference update rather than `Repository::branch`, which
        // refuses to force a branch that is already HEAD: the common case
        // here, since the worker re-syncs a branch it is already sitting on.
        repo.reference(
            &format!("refs/heads/{default}"),
            upstream.id(),
            true,
            "sync_default",
        )?;
        repo.set_head(&format!("refs/heads/{default}"))?;
        // The checkout is what clears untracked files, which a plain
        // `reset --hard` leaves behind: the worker reuses one clone across
        // issues, so whatever a killed Claude run dropped in the tree would
        // otherwise be swept into the next issue's commits by Claude.
        // Ignored files (`target/`) stay: that's the build cache.
        // `reset` can't do it in one step; it overrides the checkout strategy
        // it is handed.
        let mut checkout = CheckoutBuilder::new();
        checkout.force().remove_untracked(true);
        repo.checkout_tree(upstream.as_object(), Some(&mut checkout))?;
        repo.reset(upstream.as_object(), ResetType::Hard, None)?;
        Ok(default)
    }

    fn has_new_commits(&self, clone_path: &Path, branch: &str, base: &str) -> Result<bool> {
        let repo = Repository::open(clone_path)?;
        let head = match repo.find_branch(branch, BranchType::Local) {
            Ok(branch) => branch.into_reference().peel_to_commit()?.id(),
            // The one error worth its own answer: Claude never made the
            // branch. Anything else is a broken clone and says so.
            Err(err) if err.code() == ErrorCode::NotFound => return Ok(false),
            Err(err) => return Err(err).with_context(|| format!("looking up branch {branch}")),
        };
        let base = repo
            .find_branch(base, BranchType::Local)
            .with_context(|| format!("looking up base branch {base}"))?
            .into_reference()
            .peel_to_commit()?
            .id();
        let (ahead, _behind) = repo.graph_ahead_behind(head, base)?;
        Ok(ahead > 0)
    }

    fn push(&self, clone_path: &Path, branch: &str, token: &str) -> Result<()> {
        let repo = Repository::open(clone_path)?;
        let mut remote = repo.find_remote("origin")?;

        // libgit2 only fails `push` itself for a transport or local error. A
        // ref the remote refused (a pre-receive hook, push protection, a ref
        // it cannot write) comes back per ref through this callback, and
        // `push` still returns `Ok`: without it a refused push reads as a
        // pushed one, and the PR opens on whatever the remote had before.
        let mut refused: Vec<String> = Vec::new();
        let mut callbacks = credentials(token);
        callbacks.push_update_reference(|refname, status| {
            if let Some(message) = status {
                refused.push(format!("{refname}: {message}"));
            }
            Ok(())
        });
        let mut options = PushOptions::new();
        options.remote_callbacks(callbacks);

        remote.push(
            &[format!("refs/heads/{branch}:refs/heads/{branch}")],
            Some(&mut options),
        )?;
        drop(options);
        if !refused.is_empty() {
            anyhow::bail!("origin refused the push: {}", refused.join("; "));
        }
        Ok(())
    }
}

/// The clone at `clone_path`, cloned from `remote_url` if it isn't there yet.
///
/// Only a missing directory is cloned into: a path that exists but holds no
/// repository is a provisioning mistake, most likely two instances pointed at
/// one tree, and cloning over it would bury the evidence.
fn open_or_clone(clone_path: &Path, remote_url: &str, token: &str) -> Result<Repository> {
    if clone_path.exists() {
        return Repository::open(clone_path)
            .with_context(|| format!("opening the clone at {}", clone_path.display()));
    }
    if let Some(parent) = clone_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(credentials(token));
    RepoBuilder::new()
        .fetch_options(fetch_options)
        .clone(remote_url, clone_path)
        .with_context(|| format!("cloning {remote_url} into {}", clone_path.display()))
}

/// Callbacks that answer GitHub's HTTPS auth with the instance's OAuth token,
/// which it takes as the password behind any non-empty username. A fresh set
/// per operation: `RemoteCallbacks` is consumed by whatever it is handed to.
fn credentials(token: &str) -> RemoteCallbacks<'_> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_, _, _| Cred::userpass_plaintext("x-access-token", token));
    callbacks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use git2::{IndexAddOption, RepositoryInitOptions, Signature};
    use tempfile::TempDir;

    /// The fixture remote is a local path, which never asks for credentials.
    const TOKEN: &str = "unused-by-a-local-remote";
    /// For the fixtures that hand `sync_default` a clone that already exists,
    /// so it never reaches the clone step.
    const NO_REMOTE: &str = "never-cloned-from";

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
    fn sync_default_checks_out_the_detected_default_branch() {
        // Deliberately not `main`: the default branch has to be detected, and
        // its name is what every PR is opened against.
        let (_tmp, clone) = fixture("trunk");
        assert_eq!(
            Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap(),
            "trunk"
        );

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("trunk"));
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
    fn sync_default_clones_a_repo_that_is_not_on_the_box_yet() {
        let (tmp, clone) = fixture("main");
        let bare = tmp.path().join("remote.git");
        let fresh = tmp.path().join("nested/not/cloned/yet");
        drop(clone);

        assert_eq!(
            Git2Ops
                .sync_default(&fresh, bare.to_str().unwrap(), TOKEN)
                .unwrap(),
            "main"
        );
        assert!(fresh.join("README.md").exists());
    }

    #[test]
    fn sync_default_refuses_a_path_that_is_not_a_clone() {
        // Two instances pointed at one tree is the way this happens, and
        // cloning over it would bury the evidence.
        let tmp = TempDir::new().unwrap();
        let occupied = tmp.path().join("someone-elses-tree");
        fs::create_dir_all(&occupied).unwrap();

        assert!(
            Git2Ops
                .sync_default(&occupied, "unreachable", TOKEN)
                .is_err()
        );
    }

    #[test]
    fn sync_default_throws_away_local_work() {
        let (_tmp, clone) = fixture("main");
        Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();

        fs::write(clone.join("README.md"), "scribbled over\n").unwrap();
        fs::write(clone.join("junk.txt"), "tracked junk\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "chore: local work");
        fs::create_dir_all(clone.join("leftovers")).unwrap();
        fs::write(clone.join("leftovers/scratch.txt"), "never staged\n").unwrap();

        assert_eq!(
            Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap(),
            "main"
        );

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        assert_eq!(
            fs::read_to_string(clone.join("README.md")).unwrap(),
            "seed\n"
        );
        assert!(!clone.join("junk.txt").exists());
        assert!(!clone.join("leftovers/scratch.txt").exists());
    }

    /// Checks out `branch` off HEAD in `clone`, so the fixture can hand
    /// `has_new_commits` the two shapes a Claude run leaves behind.
    fn branch_off_head(clone: &Path, branch: &str) {
        let repo = Repository::open(clone).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch(branch, &head, true).unwrap();
        repo.set_head(&format!("refs/heads/{branch}")).unwrap();
    }

    #[test]
    fn has_new_commits_is_false_for_a_branch_claude_never_made() {
        let (_tmp, clone) = fixture("main");
        let base = Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();

        assert!(
            !Git2Ops
                .has_new_commits(&clone, "claude/issue-1", &base)
                .unwrap()
        );
    }

    #[test]
    fn has_new_commits_is_false_for_a_branch_with_nothing_on_it() {
        let (_tmp, clone) = fixture("main");
        let base = Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();
        branch_off_head(&clone, "claude/issue-1");

        assert!(
            !Git2Ops
                .has_new_commits(&clone, "claude/issue-1", &base)
                .unwrap()
        );
    }

    #[test]
    fn has_new_commits_is_true_once_something_is_committed() {
        let (_tmp, clone) = fixture("main");
        let base = Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();
        branch_off_head(&clone, "claude/issue-1");
        fs::write(clone.join("fix.txt"), "the change\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "fix: the thing");

        assert!(
            Git2Ops
                .has_new_commits(&clone, "claude/issue-1", &base)
                .unwrap()
        );
    }

    #[test]
    fn push_fails_when_the_remote_refuses_the_ref() {
        // libgit2 reports a ref the remote refused (a pre-receive hook, a
        // push rule, a ref it cannot write) per ref, not as an error from
        // `push` itself. A `claude` branch on the remote makes
        // `claude/issue-1` unwritable there, which is the local transport's
        // way of saying no.
        let (tmp, clone) = fixture("main");
        let bare = Repository::open_bare(tmp.path().join("remote.git")).unwrap();
        let head = bare.head().unwrap().peel_to_commit().unwrap();
        bare.branch("claude", &head, false).unwrap();

        Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();
        branch_off_head(&clone, "claude/issue-1");
        fs::write(clone.join("fix.txt"), "the change\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "fix: the thing");

        let err = Git2Ops
            .push(&clone, "claude/issue-1", TOKEN)
            .expect_err("a refused ref must fail the push");
        assert!(
            format!("{err:#}").contains("refs/heads/claude/issue-1"),
            "{err:#}"
        );
        assert!(bare.find_reference("refs/heads/claude/issue-1").is_err());
    }

    #[test]
    fn push_sends_the_branch_to_the_remote() {
        let (tmp, clone) = fixture("main");
        Git2Ops.sync_default(&clone, NO_REMOTE, TOKEN).unwrap();
        branch_off_head(&clone, "claude/issue-1");
        fs::write(clone.join("fix.txt"), "the change\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "fix: the thing");

        Git2Ops.push(&clone, "claude/issue-1", TOKEN).unwrap();

        let bare = Repository::open_bare(tmp.path().join("remote.git")).unwrap();
        assert_eq!(
            bare.find_reference("refs/heads/claude/issue-1")
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            Repository::open(&clone)
                .unwrap()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id()
        );
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
        Git2Ops.sync_default(&clone, &url, &token).unwrap();
        branch_off_head(&clone, &branch);
        fs::write(clone.join("push-test.txt"), "hello\n").unwrap();
        commit_all(&Repository::open(&clone).unwrap(), "test: push smoke test");
        Git2Ops.push(&clone, &branch, &token).unwrap();
    }
}
