//! Single-instance-per-label claim. Port of `worker.sh`'s `claim_label`.
//!
//! Two workers sharing one `$LABEL` both plan, both implement, and open
//! competing PRs on the same issue: the one failure mode the two-instance
//! design has no runtime defence against. Claiming the label at startup turns
//! that config mistake into a dead unit in `systemctl status` within seconds
//! instead of duplicate PRs noticed days later.
//!
//! An OS lock rather than a pid file on purpose: the kernel drops it when the
//! process dies, so there is no stale claim to reap after a crash and no
//! liveness check to get wrong. The file's contents are only a human-readable
//! label for the log line.

use std::path::{Path, PathBuf};

use fslock::LockFile;

/// Held for the worker's lifetime; dropping it releases the label.
pub struct LabelClaim {
    _lock: LockFile,
}

/// Errors the caller has to tell apart: a label another live worker holds is
/// worth a Mattermost line naming the holder, the rest are plain fatals.
pub enum ClaimError {
    Held { holder: String },
    Fatal(anyhow::Error),
}

pub fn claim_path(claim_dir: &Path, label: &str) -> PathBuf {
    claim_dir.join(format!("claudius-label-{label}.lock"))
}

pub fn claim_label(
    claim_dir: &Path,
    label: &str,
    instance: &str,
) -> Result<LabelClaim, ClaimError> {
    let path = claim_path(claim_dir, label);

    // A symlink here is either a mistake or a local user aiming our truncation
    // at a file of their choosing: refuse either way rather than follow it.
    if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_symlink()) {
        return Err(ClaimError::Fatal(anyhow::anyhow!(
            "claim path {} is a symlink, refusing to write through it",
            path.display()
        )));
    }

    let mut lock = LockFile::open(&path).map_err(|err| {
        ClaimError::Fatal(anyhow::anyhow!(
            "cannot open claim file {}: {err}",
            path.display()
        ))
    })?;

    let locked = lock.try_lock().map_err(|err| {
        ClaimError::Fatal(anyhow::anyhow!(
            "cannot lock claim file {}: {err}",
            path.display()
        ))
    })?;
    if !locked {
        return Err(ClaimError::Held {
            holder: std::fs::read_to_string(&path)
                .unwrap_or_default()
                .trim()
                .to_string(),
        });
    }

    // Whoever held this before us is gone (the kernel dropped their lock), so
    // their name in the file is stale, so overwrite it with ours.
    std::fs::write(&path, format!("{instance} (pid {})\n", std::process::id())).map_err(|err| {
        ClaimError::Fatal(anyhow::anyhow!(
            "cannot name claim file {}: {err}",
            path.display()
        ))
    })?;

    Ok(LabelClaim { _lock: lock })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cm-claim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn claims_a_free_label_and_names_itself() {
        let dir = tmpdir();
        let claim = claim_label(&dir, "free-label", "Claudius Maximus");
        assert!(claim.is_ok());
        let named = std::fs::read_to_string(claim_path(&dir, "free-label")).unwrap();
        assert!(named.starts_with("Claudius Maximus (pid "), "{named}");
    }

    #[test]
    fn a_second_claim_on_one_label_is_refused_and_names_the_holder() {
        let dir = tmpdir();
        let _first = claim_label(&dir, "contended", "Claudius Maximus")
            .ok()
            .unwrap();
        match claim_label(&dir, "contended", "Claudius Secundus") {
            Err(ClaimError::Held { holder }) => {
                assert!(holder.starts_with("Claudius Maximus (pid "), "{holder}")
            }
            _ => panic!("second instance claimed a label already being drained"),
        }
    }

    #[test]
    fn the_claim_is_per_label_so_a_second_instance_runs() {
        let dir = tmpdir();
        let _first = claim_label(&dir, "maximus", "Claudius Maximus")
            .ok()
            .unwrap();
        assert!(
            claim_label(&dir, "secundus", "Claudius Secundus").is_ok(),
            "a different label must not be blocked by another instance's claim"
        );
    }

    #[test]
    fn refuses_to_write_through_a_symlinked_claim_path() {
        let dir = tmpdir();
        let decoy = dir.join("decoy");
        std::fs::write(&decoy, "untouched").unwrap();
        let path = claim_path(&dir, "symlinked");
        let _ = std::fs::remove_file(&path);
        std::os::unix::fs::symlink(&decoy, &path).unwrap();

        assert!(matches!(
            claim_label(&dir, "symlinked", "Claudius Maximus"),
            Err(ClaimError::Fatal(_))
        ));
        assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "untouched");
    }
}
