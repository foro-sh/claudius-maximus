//! The one place `std::process::Command` runs, per #1: `claude` has no
//! Rust SDK, unlike GitHub (`cm-github`) and git (`cm-git`).
//!
//! A run is watched while it happens rather than only after it ends. A plan
//! run is minutes and an implement run can be an hour, and if the usage window
//! runs out mid-run it is hours; a worker that only learns what happened when
//! `wait_with_output` finally returns has nothing to say for all of that time,
//! which is exactly the stretch somebody watching the journal wants to hear
//! about. So both pipes are drained line by line and every line is handed to a
//! [`RunObserver`] as it arrives.

use anyhow::Context;
use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Which pipe a line came out of. Stdout is the answer (a plan, or an
/// implement run's chatter); stderr is where a run says it is in trouble,
/// including that the usage window is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Everything one `claude` invocation needs. A struct rather than five
/// positional arguments because the observer made it six, and because
/// `run(dir, model, effort, prompt)` was already one transposition away from
/// running a plan prompt at the implement model.
pub struct ClaudeRun<'a> {
    pub repo_dir: &'a Path,
    pub model: &'a str,
    pub effort: &'a str,
    pub prompt: &'a str,
    /// Told about every line, on the reading thread, while the run is still
    /// going. Must not block for long: the pipe it is holding up is the one
    /// `claude` is writing into.
    pub observer: &'a dyn RunObserver,
}

/// Watches one run as it happens.
pub trait RunObserver: Sync {
    fn line(&self, stream: Stream, line: &str);
}

/// The seam the state machine calls Claude through, so its tests can drive a
/// fake instead of spending quota (`fakes::FakeClaude`).
pub trait Claude: Send + Sync {
    fn run(&self, run: ClaudeRun<'_>) -> anyhow::Result<String>;
}

/// How much stderr is kept for the error message on a failed run. A run that
/// fails after hours of retrying inside its own watchdog can have written a
/// lot of it, and all of it was already handed to the observer line by line;
/// what the error needs is the end, which is where the reason is.
const STDERR_LINES_KEPT: usize = 200;

/// The real thing: `claude -p` on `$PATH`.
pub struct ClaudeCli;

impl Claude for ClaudeCli {
    fn run(&self, run: ClaudeRun<'_>) -> anyhow::Result<String> {
        // The prompt goes in on stdin, not as an argument: it carries the
        // issue body and the plan, and Linux caps a single argv entry at
        // 128KiB, and a long pasted log in an issue would be `E2BIG` forever.
        let mut child = Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(run.model)
            .arg("--effort")
            .arg(run.effort)
            .arg("--dangerously-skip-permissions")
            // systemd's own variables do not go to the child. A process that
            // can reach the notify socket can answer the watchdog on behalf of
            // a parent that has stopped answering, and READY=1 from a child
            // would be a lie systemd believes.
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_USEC")
            .env_remove("WATCHDOG_PID")
            .current_dir(run.repo_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Three threads, one per pipe: `claude` can fill a pipe buffer on
        // either output stream before it has read the whole prompt, so a
        // parent that does any two of these in sequence deadlocks against it
        // on the third.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let observer = run.observer;
        let prompt = run.prompt;
        let (written, out, err) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                let result = stdin.write_all(prompt.as_bytes());
                // Dropped here, not at the end of the run: `claude -p` reads
                // its prompt to EOF, so a stdin left open is a run that never
                // starts.
                drop(stdin);
                result
            });
            let out = scope.spawn(move || drain(stdout, Stream::Stdout, observer, usize::MAX));
            let err =
                scope.spawn(move || drain(stderr, Stream::Stderr, observer, STDERR_LINES_KEPT));
            (writer.join(), out.join(), err.join())
        });

        let out =
            out.map_err(|_| anyhow::anyhow!("the thread reading claude's stdout panicked"))?;
        let err =
            err.map_err(|_| anyhow::anyhow!("the thread reading claude's stderr panicked"))?;
        let status = child.wait()?;

        // Checked before the write's own result: a `claude` that exits early
        // breaks the pipe, and its status and stderr say why far better than
        // "broken pipe" does.
        if !status.success() {
            anyhow::bail!(
                "claude exited with {status}: {}",
                err.unwrap_or_default().join("\n")
            );
        }
        // A write that failed against a run that still exited 0 means claude
        // answered a prompt it only half read. The run is not trusted for
        // that reason, but nothing it committed is lost: the next sweep tells
        // it to reuse the same branch.
        match written {
            Ok(result) => result.context(
                "claude stopped reading before the whole prompt was in: it answered a \
                 truncated prompt, so the run is being retried",
            )?,
            Err(_) => anyhow::bail!("the thread feeding claude its prompt panicked"),
        }

        Ok(out?.join("\n"))
    }
}

/// Reads a pipe to EOF, handing every line to the observer as it lands and
/// keeping the last `keep` of them for the caller.
///
/// Split on bytes and converted lossily rather than read through
/// `BufRead::lines`, which gives up on the first byte that is not UTF-8. A
/// run that prints a stray byte from some tool's output has not failed, and
/// the line it printed is still worth reading.
fn drain(
    pipe: impl Read,
    stream: Stream,
    observer: &dyn RunObserver,
    keep: usize,
) -> std::io::Result<Vec<String>> {
    let mut reader = BufReader::new(pipe);
    let mut kept: VecDeque<String> = VecDeque::new();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if std::io::BufRead::read_until(&mut reader, b'\n', &mut raw)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw)
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        observer.line(stream, &line);
        kept.push_back(line);
        if kept.len() > keep {
            kept.pop_front();
        }
    }
    Ok(kept.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        lines: Mutex<Vec<(Stream, String)>>,
    }

    impl RunObserver for Recorder {
        fn line(&self, stream: Stream, line: &str) {
            self.lines.lock().unwrap().push((stream, line.to_owned()));
        }
    }

    #[test]
    fn every_line_reaches_the_observer_and_the_last_ones_are_kept() {
        let recorder = Recorder::default();
        let kept = drain("one\ntwo\nthree\n".as_bytes(), Stream::Stderr, &recorder, 2).unwrap();

        assert_eq!(kept, vec!["two".to_string(), "three".to_string()]);
        assert_eq!(
            *recorder.lines.lock().unwrap(),
            vec![
                (Stream::Stderr, "one".to_string()),
                (Stream::Stderr, "two".to_string()),
                (Stream::Stderr, "three".to_string()),
            ],
            "the cap is on what is kept, never on what is watched"
        );
    }

    #[test]
    fn a_line_that_is_not_utf8_is_still_a_line() {
        let recorder = Recorder::default();
        let kept = drain(
            &b"plan\xffned\nand done"[..],
            Stream::Stdout,
            &recorder,
            usize::MAX,
        )
        .unwrap();

        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept[0].starts_with("plan"), "{kept:?}");
        assert_eq!(
            kept[1], "and done",
            "a last line with no newline still counts"
        );
    }

    #[test]
    fn carriage_returns_do_not_become_part_of_the_line() {
        let recorder = Recorder::default();
        drain("one\r\n".as_bytes(), Stream::Stdout, &recorder, usize::MAX).unwrap();
        assert_eq!(recorder.lines.lock().unwrap()[0].1, "one");
    }
}
