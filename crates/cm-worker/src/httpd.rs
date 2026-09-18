//! The status page: one socket, four routes, no dependencies.
//!
//! The journal says what happened and Mattermost says what changed, but
//! neither answers "what is it doing *now*" without someone SSHing to the box.
//! This does, for a human (`/`), for a machine (`/status.json`), for
//! Prometheus (`/metrics`) and for a health check (`/healthz`).
//!
//! Hand-rolled HTTP, which needs a word of defence. The whole surface is four
//! GETs that render a [`Snapshot`], the alternative is an async web framework
//! and its two hundred transitive crates in a binary whose entire point is to
//! be one static file, and the worker already refuses to shell out to `git` or
//! `gh` for the same kind of reason. Connections are served one at a time on
//! one thread with read and write timeouts: the busiest this ever gets is a
//! Prometheus scrape every fifteen seconds and somebody with a browser tab
//! open.
//!
//! It is read-only in the strongest sense: there is no route that changes
//! anything, so the worst a request can do is render.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::status::{Level, Snapshot, Status, Window, human};

/// How long a client gets to send its request line and take its answer. A
/// browser tab left open must not be able to hold the only serving thread.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest request line served. Anything longer is not a request for one
/// of four fixed paths.
const REQUEST_MAX: usize = 8 * 1024;

/// How often the page refreshes itself.
const REFRESH_SECS: u32 = 5;

/// How long the accept loop waits after an error it cannot do anything about.
const ACCEPT_PAUSE: Duration = Duration::from_millis(200);

pub struct Response {
    pub code: u16,
    pub reason: &'static str,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    fn new(code: u16, reason: &'static str, content_type: &'static str, body: String) -> Self {
        Response {
            code,
            reason,
            content_type,
            body,
        }
    }

    fn write_to(&self, stream: &mut TcpStream, with_body: bool) -> std::io::Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: \
             no-store\r\nConnection: close\r\n\r\n",
            self.code,
            self.reason,
            self.content_type,
            self.body.len()
        );
        stream.write_all(head.as_bytes())?;
        if with_body {
            stream.write_all(self.body.as_bytes())?;
        }
        stream.flush()
    }
}

/// Binds the status socket and serves it from its own thread.
///
/// Binding happens here, on the caller's thread, so that a port already in use
/// (two instances handed the same one) is a worker that refuses to start with
/// a clear error, not a worker that starts and is quietly unwatchable.
pub fn serve(addr: SocketAddr, status: Arc<Status>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).map_err(|err| {
        anyhow::anyhow!(
            "cannot listen on STATUS_ADDR {addr}: {err}. Every instance on a box needs its own \
             port; add-instance.sh hands each one a different one."
        )
    })?;
    std::thread::Builder::new()
        .name("status".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => serve_one(stream, &status),
                    // One refused connection is not worth a line per attempt,
                    // and is certainly not worth ending the only thread that
                    // can answer the next one. The pause is for the errors
                    // that do not clear on their own (out of file
                    // descriptors, most of them): accepting in a tight loop
                    // against one of those would take a core away from the
                    // worker, on a box that may only have the one.
                    Err(_) => {
                        std::thread::sleep(ACCEPT_PAUSE);
                        continue;
                    }
                }
            }
        })?;
    Ok(())
}

fn serve_one(mut stream: TcpStream, status: &Status) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let Some((method, target)) = read_request(&stream) else {
        return;
    };
    let response = match method.as_str() {
        "GET" | "HEAD" => respond(&target, &status.snapshot()),
        _ => Response::new(
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            "this worker only answers questions\n".to_string(),
        ),
    };
    let _ = response.write_to(&mut stream, method != "HEAD");
}

/// The method and path of a request, or `None` if it is not one.
fn read_request(stream: &TcpStream) -> Option<(String, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?).take(REQUEST_MAX as u64);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let target = parts.next()?.to_string();
    // The rest of the headers are read and dropped: nothing here varies by
    // them, and a client that sent some deserves to have them taken off the
    // socket before the answer goes back.
    let mut header = String::new();
    while reader.read_line(&mut header).ok()? > 0 {
        if header.trim().is_empty() {
            break;
        }
        header.clear();
    }
    Some((method, target))
}

/// The whole router: a path and a snapshot in, a response out, no IO. Every
/// test below drives this directly.
pub fn respond(target: &str, snapshot: &Snapshot) -> Response {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    match path {
        "/" => Response::new(200, "OK", "text/html; charset=utf-8", page(snapshot)),
        "/status.json" => Response::new(
            200,
            "OK",
            "application/json; charset=utf-8",
            json(snapshot).to_string(),
        ),
        "/metrics" => Response::new(
            200,
            "OK",
            "text/plain; version=0.0.4; charset=utf-8",
            metrics(snapshot),
        ),
        "/healthz" => {
            let grace = grace_for(snapshot.poll_interval);
            if healthy(snapshot, grace) {
                Response::new(
                    200,
                    "OK",
                    "text/plain; charset=utf-8",
                    format!("ok: {}\n", snapshot.line()),
                )
            } else {
                Response::new(
                    503,
                    "Service Unavailable",
                    "text/plain; charset=utf-8",
                    format!(
                        "nothing has moved in {}, and no run is in flight: {}\n",
                        human(grace),
                        snapshot.line()
                    ),
                )
            }
        }
        _ => Response::new(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            "try /, /status.json, /metrics or /healthz\n".to_string(),
        ),
    }
}

/// Is the worker still moving?
///
/// Deliberately not "is it making progress": a `claude` run may legitimately
/// take hours, and the spent usage window it may be waiting out inside takes
/// five. What this catches is the case nothing else does, including the
/// systemd watchdog, which sees a process that is alive and pinging: a worker
/// that has stopped.
///
/// So: a run in flight, or an activity that changed within the grace. A sweep
/// is a long stretch of short activities - a run ends, the PR is pushed, the
/// next issue's clone is synced - and asking instead when the last sweep
/// *finished* judged all of that against a sweep that cannot have finished,
/// because it is the one still running. Any sweep longer than the grace
/// reported 503 in every gap between its runs.
///
/// A spent window is no longer an excuse of its own, and needs none: the run
/// is what waits it out, so a shut window always comes with a run in flight.
/// Standing alone it means the last run died without saying the window
/// reopened, and a worker sweeping on regardless is answered for by its
/// activity, while one that has stopped should be caught rather than excused
/// for as long as `claude` said the window would last.
pub fn healthy(snapshot: &Snapshot, grace: Duration) -> bool {
    snapshot.activity.stage.runs_claude() || snapshot.activity_for <= grace
}

/// How long `/healthz` waits for the worker to move before calling it stopped:
/// ten poll intervals, and never less than ten minutes, so that a short
/// `POLL_INTERVAL` does not turn every slow GitHub call into an outage.
pub fn grace_for(poll_interval: Duration) -> Duration {
    (poll_interval * 10).max(Duration::from_secs(600))
}

fn json(snapshot: &Snapshot) -> serde_json::Value {
    let counters = &snapshot.counters;
    serde_json::json!({
        "instance": snapshot.instance,
        "label": snapshot.label,
        "repos": snapshot.repos,
        "healthy": healthy(snapshot, grace_for(snapshot.poll_interval)),
        "line": snapshot.line(),
        "started_epoch": snapshot.started_epoch,
        "now_epoch": snapshot.now_epoch,
        "uptime_seconds": snapshot.uptime.as_secs(),
        "poll_interval_seconds": snapshot.poll_interval.as_secs(),
        "activity": {
            "stage": snapshot.activity.stage.word(),
            "subject": snapshot.activity.subject,
            "model": snapshot.activity.model,
            "seconds": snapshot.activity_for.as_secs(),
            "quiet_seconds": snapshot.quiet_for.map(|q| q.as_secs()),
            "last_line": snapshot.last_line,
        },
        "window": match &snapshot.window {
            Window::Open => serde_json::json!({ "shut": false }),
            Window::Shut { for_, resets_at, said } => serde_json::json!({
                "shut": true,
                "shut_for_seconds": for_.as_secs(),
                "resets_at_epoch": resets_at,
                "claude_said": said,
            }),
        },
        "counters": {
            "sweeps": counters.sweeps,
            "issues_seen": counters.issues_seen,
            "plans_posted": counters.plans_posted,
            "pulls_opened": counters.pulls_opened,
            "claude_runs": counters.claude_runs,
            "claude_failures": counters.claude_failures,
            "issue_failures": counters.issue_failures,
            "repo_failures": counters.repo_failures,
            "windows_shut": counters.windows_shut,
            "claude_seconds": counters.claude_time.as_secs(),
            "window_seconds": counters.window_time.as_secs(),
        },
        "last_sweep": snapshot.last_sweep.as_ref().map(|sweep| serde_json::json!({
            "ended_epoch": sweep.ended_epoch,
            "ended_ago_seconds": sweep.ended_ago.as_secs(),
            "took_seconds": sweep.took.as_secs(),
            "seen": sweep.tally.seen,
            "planned": sweep.tally.planned,
            "shipped": sweep.tally.shipped,
            "skipped": sweep.tally.skipped,
            "resting": sweep.tally.resting,
            "failed": sweep.tally.failed,
        })),
        "events": snapshot.events.iter().map(|event| serde_json::json!({
            "at_epoch": event.at_epoch,
            "level": level_word(event.level),
            "text": event.text,
        })).collect::<Vec<_>>(),
    })
}

fn level_word(level: Level) -> &'static str {
    match level {
        Level::Note => "note",
        Level::Good => "good",
        Level::Bad => "bad",
    }
}

/// Prometheus text format. The counters are the ones worth alerting on: a
/// backlog that stops shipping, a window that is shut more than it is open, a
/// run that fails every time.
fn metrics(snapshot: &Snapshot) -> String {
    let labels = format!(
        "instance=\"{}\",label=\"{}\"",
        escape_label(&snapshot.instance),
        escape_label(&snapshot.label)
    );
    let counters = &snapshot.counters;
    let mut out = String::new();
    let mut metric = |name: &str, help: &str, kind: &str, value: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name}{{{labels}}} {value}\n"
        ));
    };

    metric(
        "claudius_up",
        "1 while the worker is answering.",
        "gauge",
        "1".to_string(),
    );
    metric(
        "claudius_healthy",
        "1 while the worker is still moving: a run in flight, or an activity \
         newer than ten poll intervals.",
        "gauge",
        bit(healthy(snapshot, grace_for(snapshot.poll_interval))),
    );
    metric(
        "claudius_start_time_seconds",
        "When the worker rose, in unix seconds.",
        "gauge",
        snapshot.started_epoch.to_string(),
    );
    metric(
        "claudius_sweeps_total",
        "Sweeps completed.",
        "counter",
        counters.sweeps.to_string(),
    );
    metric(
        "claudius_issues_seen_total",
        "Labelled issues looked at, counting re-visits.",
        "counter",
        counters.issues_seen.to_string(),
    );
    metric(
        "claudius_plans_posted_total",
        "Plan comments posted.",
        "counter",
        counters.plans_posted.to_string(),
    );
    metric(
        "claudius_pull_requests_total",
        "Pull requests opened.",
        "counter",
        counters.pulls_opened.to_string(),
    );
    metric(
        "claudius_claude_runs_total",
        "claude runs started.",
        "counter",
        counters.claude_runs.to_string(),
    );
    metric(
        "claudius_claude_failures_total",
        "claude runs that came back non-zero.",
        "counter",
        counters.claude_failures.to_string(),
    );
    metric(
        "claudius_issue_failures_total",
        "Issues that failed a sweep and went into backoff.",
        "counter",
        counters.issue_failures.to_string(),
    );
    metric(
        "claudius_repo_failures_total",
        "Repos that could not be swept at all.",
        "counter",
        counters.repo_failures.to_string(),
    );
    metric(
        "claudius_claude_seconds_total",
        "Wall time spent inside claude, the wait on a spent usage window \
         included: subtract claudius_usage_window_seconds_total for the time \
         it spent working.",
        "counter",
        counters.claude_time.as_secs().to_string(),
    );
    metric(
        "claudius_usage_window_seconds_total",
        "Wall time spent waiting out spent usage windows: the throughput \
         ceiling, measured rather than guessed.",
        "counter",
        counters.window_time.as_secs().to_string(),
    );
    metric(
        "claudius_usage_windows_spent_total",
        "Usage windows spent.",
        "counter",
        counters.windows_shut.to_string(),
    );
    metric(
        "claudius_usage_window_shut",
        "1 while a spent usage window is being waited out.",
        "gauge",
        bit(snapshot.window.is_shut()),
    );
    if let Window::Shut {
        resets_at: Some(epoch),
        ..
    } = snapshot.window
    {
        metric(
            "claudius_usage_window_resets_at_seconds",
            "When claude said the window reopens, in unix seconds.",
            "gauge",
            epoch.to_string(),
        );
    }
    metric(
        "claudius_activity_seconds",
        "How long the current activity has lasted.",
        "gauge",
        snapshot.activity_for.as_secs().to_string(),
    );
    if let Some(quiet) = snapshot.quiet_for {
        metric(
            "claudius_run_quiet_seconds",
            "How long the running claude has written nothing.",
            "gauge",
            quiet.as_secs().to_string(),
        );
    }
    if let Some(sweep) = &snapshot.last_sweep {
        metric(
            "claudius_last_sweep_seconds",
            "How long ago the last sweep finished.",
            "gauge",
            sweep.ended_ago.as_secs().to_string(),
        );
        metric(
            "claudius_last_sweep_duration_seconds",
            "How long the last sweep took.",
            "gauge",
            sweep.took.as_secs().to_string(),
        );
    }
    // One series per stage rather than a number per stage: this is what makes
    // `claudius_stage{stage="implementing"}` graphable next to everything else.
    out.push_str("# HELP claudius_stage What the worker is doing, 1 for the current stage.\n");
    out.push_str("# TYPE claudius_stage gauge\n");
    for stage in crate::status::STAGES {
        out.push_str(&format!(
            "claudius_stage{{{labels},stage=\"{}\"}} {}\n",
            stage.word(),
            bit(stage == snapshot.activity.stage)
        ));
    }
    out
}

fn bit(yes: bool) -> String {
    if yes { "1" } else { "0" }.to_string()
}

/// Prometheus label values may not carry a raw quote, backslash or newline.
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

/// Text that came from outside (an issue title, a line of `claude` output, a
/// repo name) on its way onto a page.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Turns the URLs in already-escaped text into links, so that the pull request
/// a chronicle line mentions is one click away.
///
/// Escaped first, always: this text came from `claude`, which was reading an
/// issue somebody else wrote. A URL stops at the first character escaping
/// would have touched, so nothing here can reach outside an `href`.
fn linkify(escaped: &str) -> String {
    let mut out = String::new();
    let mut rest = escaped;
    while let Some(at) = rest.find("https://") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == '&' || c == '"')
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        out.push_str(&format!("<a href=\"{url}\">{url}</a>"));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The throne room: everything the register knows, on one page that refreshes
/// itself. Times go out as unix seconds in `data-` attributes and are turned
/// into local time by the browser, which is the only participant that knows
/// what timezone the person reading is in.
fn page(snapshot: &Snapshot) -> String {
    let state = match &snapshot.window {
        Window::Shut { .. } => "waiting",
        Window::Open if snapshot.activity.stage.runs_claude() => "working",
        Window::Open => "idle",
    };
    let counters = &snapshot.counters;
    let mut tiles = String::new();
    let mut tile = |name: &str, value: String| {
        tiles.push_str(&format!(
            "<div class=tile><b>{value}</b><span>{name}</span></div>"
        ));
    };
    tile("sweeps", counters.sweeps.to_string());
    tile("issues seen", counters.issues_seen.to_string());
    tile("plans", counters.plans_posted.to_string());
    tile("pull requests", counters.pulls_opened.to_string());
    tile(
        "claude runs",
        format!(
            "{} <i>/{} failed</i>",
            counters.claude_runs, counters.claude_failures
        ),
    );
    tile("in claude", human(counters.claude_time));
    tile(
        "windows spent",
        format!(
            "{} <i>/{}</i>",
            counters.windows_shut,
            human(counters.window_time)
        ),
    );
    tile(
        "failures",
        format!(
            "{} <i>/{} repos</i>",
            counters.issue_failures, counters.repo_failures
        ),
    );

    let window = match &snapshot.window {
        Window::Open => String::new(),
        Window::Shut {
            for_,
            resets_at,
            said,
        } => {
            let when = match (resets_at, said) {
                (Some(epoch), _) => format!(
                    "reopens <time data-epoch={epoch}></time> (<span data-until={epoch}>{}</span>)",
                    human(snapshot.until(*epoch))
                ),
                (None, Some(said)) => format!("claude said: {}", escape(said)),
                (None, None) => "claude did not say when it reopens".to_string(),
            };
            format!(
                "<div class=window><h2>the usage window is spent</h2><p>waiting for {} \
                 &middot; {when}</p><p class=small>Nothing is lost: the run is parked on the \
                 reset and picks up where it left off. This is the ceiling a single \
                 subscription has.</p></div>",
                human(*for_)
            )
        }
    };

    let sweep = match &snapshot.last_sweep {
        Some(sweep) => format!(
            "last sweep <time data-epoch={}></time>, {} ago, took {}: {} seen, {} planned, {} \
             shipped, {} skipped, {} resting, {} failed",
            sweep.ended_epoch,
            human(sweep.ended_ago),
            human(sweep.took),
            sweep.tally.seen,
            sweep.tally.planned,
            sweep.tally.shipped,
            sweep.tally.skipped,
            sweep.tally.resting,
            sweep.tally.failed
        ),
        None => "no sweep has finished yet".to_string(),
    };

    let heard = match &snapshot.last_line {
        Some(line) => format!("<pre class=heard>{}</pre>", escape(line)),
        None => String::new(),
    };

    // Escaped one at a time and then joined: escaping the joined string would
    // turn the separator's own entity into text.
    let repos = snapshot
        .repos
        .iter()
        .map(|repo| escape(repo))
        .collect::<Vec<_>>()
        .join(" &middot; ");

    let events = if snapshot.events.is_empty() {
        "<li class=note>nothing has happened yet</li>".to_string()
    } else {
        snapshot
            .events
            .iter()
            .rev()
            .map(|event| {
                format!(
                    "<li class={}><time data-epoch={}></time> {}</li>",
                    level_word(event.level),
                    event.at_epoch,
                    linkify(&escape(&event.text))
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    PAGE.replace("%%INSTANCE%%", &escape(&snapshot.instance))
        .replace("%%LABEL%%", &escape(&snapshot.label))
        .replace("%%REPOS%%", &repos)
        .replace("%%STATE%%", state)
        .replace("%%HEADLINE%%", &escape(&snapshot.doing()))
        .replace("%%SUBLINE%%", &escape(&snapshot.line()))
        .replace("%%WINDOW%%", &window)
        .replace("%%TILES%%", &tiles)
        .replace("%%SWEEP%%", &sweep)
        .replace("%%HEARD%%", &heard)
        .replace("%%EVENTS%%", &events)
        .replace("%%UPTIME%%", &human(snapshot.uptime))
        .replace("%%STARTED%%", &snapshot.started_epoch.to_string())
        .replace("%%REFRESH%%", &REFRESH_SECS.to_string())
}

/// The page itself. Inline, because a status page that needs a second request
/// to render is a status page that does not render when the thing it reports
/// on is the thing that is broken.
const PAGE: &str = r#"<!doctype html>
<html lang=en>
<meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<meta http-equiv=refresh content="%%REFRESH%%">
<title>%%INSTANCE%%</title>
<style>
:root {
  color-scheme: dark;
  --ink: #f4efe6; --dim: #a9a093; --edge: #2c2822;
  --bg: #14110e; --panel: #1c1815; --gold: #d9a441; --purple: #6c3b8f;
  --good: #7dbb72; --bad: #c2564a;
}
* { box-sizing: border-box; }
body {
  margin: 0; padding: 2rem 1.25rem 4rem; background: var(--bg); color: var(--ink);
  font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif;
}
main { max-width: 56rem; margin: 0 auto; }
h1 { font-size: 1.35rem; margin: 0; letter-spacing: .02em; }
h1 span { color: var(--gold); }
h2 { font-size: .95rem; margin: 0 0 .35rem; text-transform: lowercase; letter-spacing: .06em; }
.chips { color: var(--dim); font-size: .85rem; margin: .35rem 0 1.5rem; }
.chips code { color: var(--ink); background: var(--panel); padding: .1rem .4rem; border-radius: .3rem; }
.now {
  border: 1px solid var(--edge); border-left: 4px solid var(--gold); border-radius: .6rem;
  background: var(--panel); padding: 1.1rem 1.25rem;
}
.now.waiting { border-left-color: #c9762f; }
.now.idle { border-left-color: #4a443c; }
.now b { display: block; font-size: 1.5rem; font-weight: 600; }
.now p { margin: .3rem 0 0; color: var(--dim); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .85rem; }
.window { margin-top: 1rem; border: 1px solid #573a1c; background: #241a10; border-radius: .6rem; padding: 1rem 1.25rem; }
.window h2 { color: #e0a04a; }
.window p { margin: .2rem 0 0; }
.small { color: var(--dim); font-size: .82rem; }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(9.5rem, 1fr)); gap: .6rem; margin: 1rem 0; }
.tile { background: var(--panel); border: 1px solid var(--edge); border-radius: .5rem; padding: .7rem .85rem; }
.tile b { display: block; font-size: 1.2rem; }
.tile i { font-style: normal; color: var(--dim); font-size: .8rem; }
.tile span { color: var(--dim); font-size: .78rem; text-transform: lowercase; letter-spacing: .04em; }
.heard { background: #100d0a; border: 1px solid var(--edge); border-radius: .5rem; padding: .6rem .8rem; overflow-x: auto; color: var(--dim); font-size: .82rem; }
ul { list-style: none; margin: .4rem 0 0; padding: 0; }
li { border-bottom: 1px solid var(--edge); padding: .4rem 0; font-size: .88rem; }
li time { color: var(--dim); font-variant-numeric: tabular-nums; margin-right: .6rem; }
li.good { border-left: 3px solid var(--good); padding-left: .6rem; }
li.bad { border-left: 3px solid #c2564a; padding-left: .6rem; }
li.note { border-left: 3px solid var(--edge); padding-left: .6rem; }
footer { margin-top: 2rem; color: var(--dim); font-size: .82rem; }
footer a { color: var(--gold); }
</style>
<main>
  <h1><span>&#9819;</span> %%INSTANCE%%</h1>
  <div class=chips>draining <code>%%LABEL%%</code> in %%REPOS%%</div>

  <div class="now %%STATE%%">
    <b>%%HEADLINE%%</b>
    <p>%%SUBLINE%%</p>
  </div>
  %%WINDOW%%
  %%HEARD%%

  <div class=tiles>%%TILES%%</div>
  <div class=small>%%SWEEP%%</div>

  <h2 style="margin-top:1.6rem">chronicle</h2>
  <ul>%%EVENTS%%</ul>

  <footer>
    up %%UPTIME%%, since <time data-epoch=%%STARTED%%></time> &middot;
    <a href=/status.json>status.json</a> &middot;
    <a href=/metrics>metrics</a> &middot;
    <a href=/healthz>healthz</a> &middot;
    refreshes every %%REFRESH%%s
  </footer>
</main>
<script>
for (const el of document.querySelectorAll('time[data-epoch]')) {
  el.textContent = new Date(el.dataset.epoch * 1000).toLocaleString();
}
for (const el of document.querySelectorAll('[data-until]')) {
  const tick = () => {
    const left = Math.max(0, el.dataset.until - Math.floor(Date.now() / 1000));
    const h = Math.floor(left / 3600), m = Math.floor((left % 3600) / 60), s = left % 60;
    el.textContent = h ? `${h}h${String(m).padStart(2, '0')}m` : `${m}m${String(s).padStart(2, '0')}s`;
  };
  tick();
  setInterval(tick, 1000);
}
</script>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::limits::Spent;
    use crate::status::{Activity, LastSweep, Stage, Status, Tally};

    fn snapshot() -> Snapshot {
        Status::new(&Config::sample()).snapshot()
    }

    fn working() -> Snapshot {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(
            Stage::Implementing,
            "foro-sh/foro#7",
            "claude-sonnet-5",
        ));
        status.heard("running cargo test --workspace");
        status.snapshot()
    }

    fn body(target: &str, snapshot: &Snapshot) -> String {
        respond(target, snapshot).body
    }

    #[test]
    fn the_four_routes_answer_and_anything_else_says_so() {
        let snapshot = working();
        for (target, code, content_type) in [
            ("/", 200, "text/html; charset=utf-8"),
            ("/status.json", 200, "application/json; charset=utf-8"),
            ("/metrics", 200, "text/plain; version=0.0.4; charset=utf-8"),
            ("/healthz", 200, "text/plain; charset=utf-8"),
        ] {
            let response = respond(target, &snapshot);
            assert_eq!(response.code, code, "{target}");
            assert_eq!(response.content_type, content_type, "{target}");
            assert!(!response.body.is_empty(), "{target}");
        }

        let missing = respond("/wp-login.php", &snapshot);
        assert_eq!(missing.code, 404);
        assert!(missing.body.contains("/status.json"));
    }

    #[test]
    fn a_query_string_does_not_invent_a_new_route() {
        assert_eq!(respond("/status.json?pretty=1", &working()).code, 200);
        assert_eq!(respond("/?from=grafana#top", &working()).code, 200);
    }

    #[test]
    fn the_json_says_what_is_running_and_what_has_happened() {
        let snapshot = working();
        let json: serde_json::Value =
            serde_json::from_str(&body("/status.json", &snapshot)).unwrap();

        assert_eq!(json["instance"], "Claudius Maximus");
        assert_eq!(json["label"], "claudius-maximus");
        assert_eq!(json["activity"]["stage"], "implementing");
        assert_eq!(json["activity"]["subject"], "foro-sh/foro#7");
        assert_eq!(json["activity"]["model"], "claude-sonnet-5");
        assert_eq!(
            json["activity"]["last_line"],
            "running cargo test --workspace"
        );
        assert_eq!(json["window"]["shut"], false);
        assert_eq!(json["healthy"], true);
        assert_eq!(json["counters"]["pulls_opened"], 0);
    }

    #[test]
    fn a_spent_window_is_in_every_rendering() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        status.window_shut(&Spent {
            resets_at: Some(crate::status::now_epoch() + 3600),
            said: Some("reset at 3pm".to_string()),
        });
        let snapshot = status.snapshot();

        let json: serde_json::Value =
            serde_json::from_str(&body("/status.json", &snapshot)).unwrap();
        assert_eq!(json["window"]["shut"], true);
        assert!(json["window"]["resets_at_epoch"].is_number());

        let metrics = body("/metrics", &snapshot);
        assert!(metrics.contains(
            "claudius_usage_window_shut{instance=\"Claudius Maximus\",label=\"claudius-maximus\"} 1"
        ));
        assert!(metrics.contains("claudius_usage_window_resets_at_seconds"));

        let page = body("/", &snapshot);
        assert!(page.contains("the usage window is spent"), "{page}");
        assert!(page.contains("class=\"now waiting\""), "{page}");
    }

    #[test]
    fn metrics_name_the_current_stage_and_nothing_else() {
        let metrics = body("/metrics", &working());
        assert!(metrics.contains("claudius_stage{instance=\"Claudius Maximus\",label=\"claudius-maximus\",stage=\"implementing\"} 1"));
        assert!(metrics.contains("stage=\"resting\"} 0"));
        assert!(metrics.contains("claudius_run_quiet_seconds"));
        assert!(
            !metrics.contains("claudius_last_sweep_seconds"),
            "no sweep has finished, so there is no gauge to report"
        );
    }

    #[test]
    fn every_metrics_line_is_a_comment_or_a_sample() {
        // A HELP string is written across source lines for the sake of the
        // margin, and a newline that survived into one would end the comment
        // and leave the rest of the sentence parsed as a metric.
        for line in body("/metrics", &working()).lines() {
            assert!(
                line.starts_with("# HELP ")
                    || line.starts_with("# TYPE ")
                    || line.starts_with("claudius_"),
                "stray line in /metrics: {line:?}"
            );
        }
    }

    #[test]
    fn a_label_value_cannot_break_out_of_its_quotes() {
        let mut snapshot = snapshot();
        snapshot.instance = "Claudius \"the \\ Great\"".to_string();
        let metrics = body("/metrics", &snapshot);

        assert!(
            metrics.contains(r#"instance="Claudius \"the \\ Great\"""#),
            "{metrics}"
        );
    }

    #[test]
    fn claude_s_own_output_cannot_write_html() {
        let status = Status::new(&Config::sample());
        status.doing(Activity::run(Stage::Planning, "foro-sh/foro#7", "opus"));
        // An issue body wrote most of the prompt that produced this line.
        status.heard("<script>alert('the senate')</script>");
        status.note(
            crate::status::Level::Bad,
            "<img src=x onerror=alert(1)> failed",
        );

        let page = body("/", &status.snapshot());
        assert!(!page.contains("<script>alert"), "{page}");
        assert!(page.contains("&lt;script&gt;alert"), "{page}");
        assert!(page.contains("&lt;img src=x"), "{page}");
    }

    #[test]
    fn a_pull_request_in_the_chronicle_is_a_link() {
        let status = Status::new(&Config::sample());
        status.note(
            crate::status::Level::Good,
            "shipped foro-sh/foro#7: https://github.com/foro-sh/foro/pull/44",
        );
        let page = body("/", &status.snapshot());

        assert!(
            page.contains(
                "<a href=\"https://github.com/foro-sh/foro/pull/44\">https://github.com/foro-sh/foro/pull/44</a>"
            ),
            "{page}"
        );
    }

    #[test]
    fn nothing_but_an_https_url_becomes_a_link() {
        let nasty = escape("javascript:alert(1) and \"https://evil onerror=x");
        let linked = linkify(&nasty);

        assert!(!linked.contains("javascript:alert(1)</a>"), "{linked}");
        assert!(linked.contains("<a href=\"https://evil\">"), "{linked}");
        assert!(!linked.contains("onerror=x</a>"), "{linked}");
    }

    #[test]
    fn healthz_is_about_the_loop_not_about_speed() {
        let mut snapshot = snapshot();
        let grace = grace_for(snapshot.poll_interval);

        snapshot.activity = Activity::run(Stage::Implementing, "foro-sh/foro#7", "sonnet");
        snapshot.activity_for = Duration::from_secs(9 * 3600);
        snapshot.uptime = Duration::from_secs(9 * 3600);
        assert!(
            healthy(&snapshot, grace),
            "a nine-hour implement run is slow, not sick"
        );

        snapshot.activity = Activity::bare(Stage::Resting);
        snapshot.activity_for = Duration::from_secs(30);
        assert!(
            healthy(&snapshot, grace),
            "resting between sweeps, having moved 30 seconds ago"
        );

        snapshot.activity_for = grace + Duration::from_secs(1);
        assert!(
            !healthy(&snapshot, grace),
            "nine hours up, no run in flight, nothing has moved: the loop stopped"
        );
        assert_eq!(respond("/healthz", &snapshot).code, 503);
        assert!(body("/healthz", &snapshot).contains("nothing has moved"));
    }

    #[test]
    fn a_long_sweep_is_healthy_between_its_runs() {
        // The gap between one issue's run and the next: the PR is pushed, the
        // labels are moved, the next clone is synced. No claude in flight, and
        // the sweep that would prove the loop is turning is the one still
        // running, hours in. Judging that against the *last finished* sweep
        // answered 503 in every such gap, on the workload this exists to watch.
        let mut snapshot = snapshot();
        let grace = grace_for(snapshot.poll_interval);

        snapshot.activity = Activity::on(Stage::Shipping, "foro-sh/foro#7");
        snapshot.activity_for = Duration::from_secs(4);
        snapshot.uptime = Duration::from_secs(3 * 3600);
        snapshot.last_sweep = Some(LastSweep {
            tally: Tally::default(),
            took: Duration::from_secs(60),
            ended_epoch: snapshot.now_epoch - 3 * 3600,
            ended_ago: Duration::from_secs(3 * 3600),
        });

        assert!(healthy(&snapshot, grace));
        assert_eq!(respond("/healthz", &snapshot).code, 200);
    }

    #[test]
    fn a_window_nobody_is_waiting_out_does_not_excuse_a_stopped_loop() {
        // A run that died for some other reason leaves the shut window behind
        // it: `claude` is what waits a window out, so a shut one with no run
        // in flight is a leftover, not a worker doing its job. Excusing it
        // would have hidden a stopped loop for as long as claude said the
        // window would last.
        let mut snapshot = snapshot();
        let grace = grace_for(snapshot.poll_interval);
        snapshot.activity = Activity::bare(Stage::Resting);
        snapshot.activity_for = grace + Duration::from_secs(1);
        snapshot.window = Window::Shut {
            for_: Duration::from_secs(4 * 3600),
            resets_at: None,
            said: None,
        };

        assert!(!healthy(&snapshot, grace));
        assert_eq!(respond("/healthz", &snapshot).code, 503);

        // Inside the run that is actually waiting it out, it is healthy.
        snapshot.activity = Activity::run(Stage::Implementing, "foro-sh/foro#7", "sonnet");
        assert!(healthy(&snapshot, grace));
    }

    #[test]
    fn the_grace_never_drops_below_ten_minutes() {
        assert_eq!(
            grace_for(Duration::from_secs(1)),
            Duration::from_secs(600),
            "a one-second poll interval must not make every hiccup an outage"
        );
        assert_eq!(
            grace_for(Duration::from_secs(300)),
            Duration::from_secs(3000)
        );
    }

    #[test]
    fn several_repos_are_listed_without_the_separator_showing_through() {
        let mut snapshot = snapshot();
        snapshot.repos = vec!["foro-sh/foro".to_string(), "foro-sh/<i>".to_string()];
        let page = body("/", &snapshot);

        assert!(
            page.contains("foro-sh/foro &middot; foro-sh/&lt;i&gt;"),
            "{page}"
        );
        assert!(!page.contains("&amp;middot;"), "{page}");
    }

    #[test]
    fn the_page_carries_everything_it_needs_to_render_itself() {
        let page = body("/", &working());
        assert!(page.starts_with("<!doctype html>"));
        assert!(!page.contains("%%"), "every slot is filled: {page}");
        assert!(
            !page.contains("src=http") && !page.contains("src=\"http"),
            "a status page must not need the network it is reporting on"
        );
        assert!(page.contains("implementing foro-sh/foro#7"));
    }

    #[test]
    fn a_request_is_read_off_the_socket_and_answered() {
        let status = Arc::new(Status::new(&Config::sample()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_one(stream, &status);
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nUser-Agent: test\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();

        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(
            answer.contains("Content-Type: text/plain; charset=utf-8"),
            "{answer}"
        );
        assert!(answer.contains("ok: rising"), "{answer}");
    }

    #[test]
    fn a_request_that_is_not_a_question_is_refused() {
        let status = Arc::new(Status::new(&Config::sample()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_one(stream, &status);
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .write_all(b"POST /status.json HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();

        assert!(
            answer.starts_with("HTTP/1.1 405 Method Not Allowed"),
            "{answer}"
        );
    }

    #[test]
    fn two_instances_cannot_be_handed_the_same_port() {
        let status = Arc::new(Status::new(&Config::sample()));
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();

        let err = serve(addr, status).expect_err("the port is already taken");
        assert!(
            err.to_string().contains("its own port"),
            "the error has to say what to do about it: {err}"
        );
    }
}
