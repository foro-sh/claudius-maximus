//! The one place `std::process::Command` runs, per #1 — `claude` has no
//! Rust SDK, unlike GitHub (`cm-github`) and git (`cm-git`).
use anyhow::Context;
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

        // Written from its own thread, and joined only after the output has
        // been drained: `claude` can write more than a pipe buffer's worth of
        // output before it has read the whole prompt, so a parent that
        // finishes writing before it starts reading deadlocks against it.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let (output, written) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(prompt.as_bytes()));
            let output = child.wait_with_output();
            (output, writer.join())
        });
        let output = output?;

        // Checked before the write's own result: a `claude` that exits early
        // breaks the pipe, and its status and stderr say why far better than
        // "broken pipe" does.
        if !output.status.success() {
            anyhow::bail!(
                "claude exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        match written {
            Ok(result) => result.context("feeding claude its prompt")?,
            Err(_) => anyhow::bail!("the thread feeding claude its prompt panicked"),
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
