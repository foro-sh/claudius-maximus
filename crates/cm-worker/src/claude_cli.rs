//! The one place `std::process::Command` runs, per #1 — `claude` has no
//! Rust SDK, unlike GitHub (`cm-github`) and git (`cm-git`).
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The seam the state machine calls Claude through, so its tests can drive a
/// fake instead of spending quota (`fakes::FakeClaude`).
pub trait Claude: Send + Sync {
    fn run(
        &self,
        repo_dir: &Path,
        model: &str,
        effort: &str,
        prompt: &str,
    ) -> anyhow::Result<String>;
}

/// The real thing: `claude -p` on `$PATH`.
pub struct ClaudeCli;

impl Claude for ClaudeCli {
    fn run(
        &self,
        repo_dir: &Path,
        model: &str,
        effort: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        // The prompt goes in on stdin, not as an argument: it carries the
        // issue body and the plan, and Linux caps a single argv entry at
        // 128KiB — a long pasted log in an issue would be `E2BIG` forever.
        let mut child = Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(model)
            .arg("--effort")
            .arg(effort)
            .arg("--dangerously-skip-permissions")
            .current_dir(repo_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Written from its own thread: a prompt larger than the pipe buffer
        // would otherwise block here while `claude` blocks writing output
        // nobody is reading yet.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let written = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(prompt.as_bytes()));
            writer.join()
        });
        match written {
            Ok(result) => result?,
            Err(_) => anyhow::bail!("the thread feeding claude its prompt panicked"),
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            anyhow::bail!(
                "claude exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
