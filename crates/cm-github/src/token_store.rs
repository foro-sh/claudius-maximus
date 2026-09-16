//! Where the OAuth token lives between runs.
//!
//! A file in the instance's own home, not an OS keychain: the worker runs as a
//! `nologin` unix user under systemd, with no login session, no D-Bus and no
//! Secret Service for a keychain to live in — `keyring` would have nothing to
//! talk to on the box this ships to. `$HOME` is already the trust boundary
//! that holds the instance's Claude subscription credentials, so the GitHub
//! token sits beside them, readable only by that user.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;

pub(crate) trait TokenStore: Send + Sync {
    /// The stored token, or `None` if there isn't one yet.
    fn load(&self) -> anyhow::Result<Option<String>>;
    fn store(&self, token: &str) -> anyhow::Result<()>;
    /// Where the token is kept, for the message that tells an operator which
    /// one to delete when GitHub stops accepting it.
    fn location(&self) -> String;
}

/// A `0600` file under the instance's home.
pub(crate) struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// `$HOME/.claudius-maximus/github-token`. One instance per unix user, so
    /// the home directory is what keeps two instances' tokens apart.
    pub(crate) fn in_home() -> anyhow::Result<Self> {
        let home = std::env::var_os("HOME")
            .context("HOME is unset, so there is nowhere to keep the GitHub token")?;
        Ok(Self::at(
            Path::new(&home)
                .join(".claudius-maximus")
                .join("github-token"),
        ))
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        FileStore { path }
    }
}

impl TokenStore for FileStore {
    fn load(&self) -> anyhow::Result<Option<String>> {
        match fs::read_to_string(&self.path) {
            // An empty file is a half-written one — `store` truncates before
            // it writes, so a kill or a full disk leaves zero bytes behind.
            // Reading that back as a token would send GitHub an empty bearer,
            // be refused, and park the worker on "delete the stored token" for
            // a file that holds nothing. No token is exactly what it is.
            Ok(token) => Ok(Some(token.trim().to_owned()).filter(|t| !t.is_empty())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn store(&self, token: &str) -> anyhow::Result<()> {
        let dir = self
            .path
            .parent()
            .context("the token path has no parent directory")?;

        // A symlink at either end aims our write at something someone else
        // chose — refuse rather than follow it, the same way the label claim
        // does. `create_dir_all` and `set_permissions` both follow links, so
        // the directory needs the check as much as the file does.
        refuse_symlink(dir)?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("tightening {}", dir.display()))?;
        refuse_symlink(&self.path)?;

        // Written beside the real file and renamed over it, so an interrupted
        // write leaves the old token intact rather than a truncated file. A
        // half-written token would read back as no token at all, and an
        // unattended worker would then sit on a device-flow prompt nobody is
        // there to answer.
        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("opening {}", temporary.display()))?;
        // `mode` above is ignored when the file already exists — a leftover
        // from an earlier crash — so tighten before the token goes in rather
        // than after.
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("tightening {}", temporary.display()))?;
        file.write_all(token.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .with_context(|| format!("writing {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("renaming {} into place", temporary.display()))
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }
}

/// Refuses a path that is a symlink. A path that does not exist yet is fine —
/// it is the redirect we are looking for, not the absence.
fn refuse_symlink(path: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink()) {
        anyhow::bail!(
            "{} is a symlink — refusing to write through it",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(home: &TempDir) -> FileStore {
        FileStore::at(home.path().join(".claudius-maximus").join("github-token"))
    }

    #[test]
    fn nothing_stored_yet_is_not_an_error() {
        let home = TempDir::new().unwrap();
        assert_eq!(store(&home).load().unwrap(), None);
    }

    #[test]
    fn a_stored_token_comes_back_and_only_the_owner_can_read_it() {
        let home = TempDir::new().unwrap();
        let store = store(&home);
        store.store("gho_token").unwrap();

        assert_eq!(store.load().unwrap().as_deref(), Some("gho_token"));
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(Path::new(&store.location())), 0o600);
        assert_eq!(mode(&home.path().join(".claudius-maximus")), 0o700);
    }

    #[test]
    fn a_half_written_token_file_reads_as_no_token_at_all() {
        let home = TempDir::new().unwrap();
        let store = store(&home);
        store.store("gho_token").unwrap();
        fs::write(store.location(), "").unwrap();

        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn re_authorizing_replaces_the_token_rather_than_appending_to_it() {
        let home = TempDir::new().unwrap();
        let store = store(&home);
        store.store("a-much-longer-old-token").unwrap();
        store.store("gho_new").unwrap();

        assert_eq!(store.load().unwrap().as_deref(), Some("gho_new"));
    }

    #[test]
    fn a_leftover_temporary_file_is_reused_without_widening_it() {
        let home = TempDir::new().unwrap();
        let store = store(&home);
        let temporary = home
            .path()
            .join(".claudius-maximus")
            .join("github-token.tmp");
        fs::create_dir_all(temporary.parent().unwrap()).unwrap();
        fs::write(&temporary, "leftover").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();

        store.store("gho_token").unwrap();

        assert_eq!(store.load().unwrap().as_deref(), Some("gho_token"));
        assert_eq!(
            fs::metadata(store.location()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            !temporary.exists(),
            "the temporary file is renamed, not left"
        );
    }

    #[test]
    fn refuses_to_write_through_a_symlinked_directory() {
        let home = TempDir::new().unwrap();
        let elsewhere = home.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.path().join(".claudius-maximus")).unwrap();

        assert!(store(&home).store("gho_token").is_err());
        assert!(!elsewhere.join("github-token").exists());
    }

    #[test]
    fn refuses_to_write_through_a_symlinked_token_path() {
        let home = TempDir::new().unwrap();
        let store = store(&home);
        let decoy = home.path().join("decoy");
        fs::write(&decoy, "untouched").unwrap();
        fs::create_dir_all(home.path().join(".claudius-maximus")).unwrap();
        std::os::unix::fs::symlink(&decoy, home.path().join(".claudius-maximus/github-token"))
            .unwrap();

        assert!(store.store("gho_token").is_err());
        assert_eq!(fs::read_to_string(&decoy).unwrap(), "untouched");
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::TokenStore;
    use std::sync::Mutex;

    /// In-memory stand-in so the client's tests never touch the filesystem.
    #[derive(Default)]
    pub(crate) struct MemoryStore(Mutex<Option<String>>);

    impl MemoryStore {
        pub(crate) fn with_token(token: &str) -> Self {
            let this = Self::default();
            this.store(token).unwrap();
            this
        }

        pub(crate) fn get(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl TokenStore for MemoryStore {
        fn load(&self) -> anyhow::Result<Option<String>> {
            Ok(self.get())
        }

        fn store(&self, token: &str) -> anyhow::Result<()> {
            *self.0.lock().unwrap() = Some(token.to_owned());
            Ok(())
        }

        fn location(&self) -> String {
            "<memory>".to_owned()
        }
    }
}
