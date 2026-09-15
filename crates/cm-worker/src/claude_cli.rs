//! The one place `std::process::Command` runs, per #1 — `claude` has no
//! Rust SDK, unlike GitHub (`cm-github`) and git (`cm-git`).
use std::path::Path;
use std::process::Command;

pub fn run(repo_dir: &Path, model: &str, effort: &str, prompt: &str) -> anyhow::Result<String> {
    let output = Command::new("claude")
        .arg("-p")
        .arg("--model")
        .arg(model)
        .arg("--effort")
        .arg(effort)
        .arg("--dangerously-skip-permissions")
        .arg(prompt)
        .current_dir(repo_dir)
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "claude exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
