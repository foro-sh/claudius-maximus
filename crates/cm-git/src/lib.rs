//! The trait `cm-worker` drives git through, plus its `git2` implementation.
//! Kept as a trait so the worker's state machine can also be tested against a
//! fake (foro-sh/claudius-maximus#1).
use std::path::Path;

use anyhow::{Context, Result, bail};
use git2::{
    BranchType, Cred, Direction, FetchOptions, IndexAddOption, PushOptions, RemoteCallbacks,
    Repository, ResetType, Signature,
};

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
        repo.reset(upstream.as_object(), ResetType::Hard, None)?;
        Ok(())
    }

    fn commit(
        &self,
        clone_path: &Path,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<Option<String>> {
        validate_conventional_commit(message)?;

        let repo = Repository::open(clone_path)?;
        let mut index = repo.index()?;
        index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
        index.write()?;

        let tree_id = index.write_tree()?;
        let head = repo.head()?.peel_to_commit()?;
        if head.tree_id() == tree_id {
            return Ok(None);
        }

        let tree = repo.find_tree(tree_id)?;
        let author = Signature::now(author_name, author_email)?;
        let oid = repo.commit(Some("HEAD"), &author, &author, message, &tree, &[&head])?;
        Ok(Some(oid.to_string()))
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

const TYPES: [&str; 9] = [
    "feat", "fix", "chore", "ci", "docs", "style", "refactor", "perf", "test",
];

/// A faithful-enough subset of `@commitlint/config-conventional`: it rejects
/// what commitlint's CI job would reject, so a libgit2 commit (which never
/// fires `commit-msg`) can't sneak a bad message past it.
fn validate_conventional_commit(message: &str) -> Result<()> {
    let header = message.lines().next().unwrap_or_default();
    if header.chars().count() > 100 {
        bail!("header longer than 100 characters: {header:?}");
    }

    let (prefix, description) = header
        .split_once(": ")
        .with_context(|| format!("header is not `type(scope): description`: {header:?}"))?;
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);

    let (kind, scope) = match prefix.split_once('(') {
        Some((kind, rest)) => (
            kind,
            Some(
                rest.strip_suffix(')')
                    .with_context(|| format!("unterminated scope: {header:?}"))?,
            ),
        ),
        None => (prefix, None),
    };

    if !TYPES.contains(&kind) {
        bail!("type {kind:?} is not one of {TYPES:?}");
    }
    if scope.is_some_and(str::is_empty) {
        bail!("empty scope: {header:?}");
    }
    if description.is_empty() {
        bail!("empty description: {header:?}");
    }
    if description.ends_with('.') {
        bail!("description ends with a period: {header:?}");
    }
    if description.starts_with(char::is_uppercase) {
        bail!("description starts with an uppercase letter: {header:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use git2::RepositoryInitOptions;
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

        Git2Ops.sync_branch(&clone, "cm/issue-2").unwrap();

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("cm/issue-2"));
        assert_eq!(
            fs::read_to_string(clone.join("README.md")).unwrap(),
            "seed\n"
        );
        assert!(!clone.join("junk.txt").exists());
    }

    #[test]
    fn commit_stages_everything_and_returns_the_new_sha() {
        let (_tmp, clone) = fixture("main");
        fs::write(clone.join("new.txt"), "hello\n").unwrap();
        fs::remove_file(clone.join("README.md")).unwrap();

        let sha = Git2Ops
            .commit(&clone, "feat: add new file", "Bot", "bot@example.com")
            .unwrap()
            .expect("something was staged");

        let repo = Repository::open(&clone).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id().to_string(), sha);
        assert_eq!(head.message(), Some("feat: add new file"));
        let tree = head.tree().unwrap();
        assert!(tree.get_name("new.txt").is_some());
        assert!(tree.get_name("README.md").is_none());
    }

    #[test]
    fn commit_is_none_when_nothing_changed() {
        let (_tmp, clone) = fixture("main");
        let committed = Git2Ops
            .commit(&clone, "feat: nothing to see", "Bot", "bot@example.com")
            .unwrap();
        assert!(committed.is_none());
    }

    #[test]
    fn commit_rejects_a_bad_message_before_touching_the_repo() {
        let (_tmp, clone) = fixture("main");
        fs::write(clone.join("new.txt"), "hello\n").unwrap();

        Git2Ops
            .commit(&clone, "Added a new file.", "Bot", "bot@example.com")
            .unwrap_err();

        let repo = Repository::open(&clone).unwrap();
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("chore: seed")
        );
    }

    #[test]
    fn accepts_what_commitlint_accepts() {
        for message in [
            "feat: add device-flow login",
            "feat(worker): add device-flow login",
            "fix(cm-git): reset onto origin's default branch",
            "chore!: drop the bash worker",
            "refactor(worker)!: split the state machine",
            "docs: describe the label state machine\n\nBody paragraph here.",
            "test: cover the conventional-commit validator",
        ] {
            validate_conventional_commit(message)
                .unwrap_or_else(|e| panic!("{message:?} should be valid: {e}"));
        }
    }

    #[test]
    fn rejects_what_commitlint_rejects() {
        for message in [
            "",
            "add device-flow login",          // no type
            "feat:add device-flow login",     // no space after the colon
            "feat add device-flow login",     // no colon
            "Feat: add device-flow login",    // uppercase type
            "wip: add device-flow login",     // type not in the allowed set
            "feature: add device-flow login", // near-miss type
            "feat(): add device-flow login",  // empty scope
            "feat(worker: add device-flow",   // unterminated scope
            "feat: Add device-flow login",    // sentence-case description
            "feat: add device-flow login.",   // trailing period
            "feat: ",                         // empty description
        ] {
            assert!(
                validate_conventional_commit(message).is_err(),
                "{message:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_overlong_header() {
        let message = format!("feat: {}", "x".repeat(95));
        assert!(validate_conventional_commit(&message).is_err());
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
        Git2Ops
            .commit(&clone, "test: push smoke test", "Bot", "bot@example.com")
            .unwrap()
            .unwrap();
        Git2Ops.push(&clone, &branch, &token).unwrap();
    }
}
