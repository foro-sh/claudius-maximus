# Claudius Maximus

<p align="center">
  <img src="assets/claudius-maximus.jpg" alt="Claudius Maximus" width="100%">
</p>

A long-running binary that turns labeled GitHub issues into PRs with Claude
Code, plan-then-implement, on a **subscription** (OAuth, not
`ANTHROPIC_API_KEY`) — the goal being to keep the rolling 5h usage window busy.
No webhooks, no queue, no database: a poll loop over the GitHub API with GitHub
labels as the only state. Everything is outbound, so the box needs no inbound
tunnel.

One **instance** = one subscription = one unix user, run from the
`claudius@.service` template. More throughput means more instances, each
draining its own label; `add-instance.sh` provisions them.

It started as a bash script in a private monorepo and is being ported to Rust
here: `octocrab` for GitHub over device-flow OAuth, `git2` for git, and
`Command` reserved for the `claude` CLI alone — so the box needs neither `gh`
nor `git`. The port is tracked in
[#1](https://github.com/foro-sh/claudius-maximus/issues/1); this README
describes the binary that issue specifies.

## How it works

| Issue state                                     | Worker action                                                                                                                                  |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Author outside the repo's allowlist             | Skipped before planning                                                                                                                        |
| Blocked by an open issue (GitHub relationship)  | Skipped until every blocker closes                                                                                                             |
| Labeled `$LABEL`, not yet `:planned`            | Claude posts an implementation plan, adds `$LABEL:planned`                                                                                      |
| `:planned`                                      | Claude implements and commits on `claude/issue-N`; the worker pushes it and opens the PR (`Closes #N`), adds `:done`, removes the trigger label |
| `:done`                                         | Ignored                                                                                                                                        |

A reboot loses nothing — all state is in labels.

When the usage window runs out, Claude handles it itself
(`CLAUDE_CODE_RETRY_WATCHDOG=1`): it waits for the reset and resumes the run it
was in. The worker doesn't manage that wait, it only reports it — see
[the usage window](#the-usage-window).

### Multiple repos

`REPOS` takes a list, and one worker serves all of it: a sweep walks the repos
in order, drains each backlog, then sleeps. Adding a repo widens what that one
worker looks at; it does not start a second Claude process. Each repo needs its
own clone and its own copy of the labels. Since issue numbers repeat across
repos, logs and Mattermost lines are qualified: `foro-sh/foro#12`.

### Who may file an issue

A `$REPOS` entry can end in a `|`-separated allowlist of issue **authors**
(matched case-insensitively):

```
foro-sh/foro=/home/claudius-maximus/repos/foro=danielsteman|thijssdaniels
```

A **public** repo needs this: without it, a stranger's issue becomes a Claude
prompt the moment someone applies the label. A private repo can go without —
filing already requires access. Applying the trigger label is the only gate;
there is no separate approval step.

## Setup

`add-instance.sh` provisions an instance: the unix user, its env file, and the
unit. Run it as root from a checkout, once per subscription. Use it for the
first instance too — it keeps the user, label, and display name in sync for you.

```bash
# Every release ships a static linux/x86_64 binary; use it if the box has no
# Rust toolchain:
#   curl -fsSLo claudius-maximus https://github.com/foro-sh/claudius-maximus/releases/latest/download/claudius-maximus-linux-x86_64
#   chmod +x claudius-maximus && export CLAUDIUS_BINARY=$PWD/claudius-maximus
cargo build --release

sudo env REPOS=foro-sh/claudius-maximus \
         GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx \
         GIT_AUTHOR_NAME="Person 1" \
         GIT_AUTHOR_EMAIL=<verified-email-on-that-github-account> \
         CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx \
    ./add-instance.sh
```

With no name argument it takes the next free slot from a fixed Latin-ordinal
list (`claudius-maximus`, `claudius-secundus`, … `claudius-centesimus`, cap
100); pass a name explicitly only to re-run or repair that slot. Re-running a
named slot is safe — an existing user or env file is left alone.

It validates before it creates anything: a `$REPOS` typo, a relative clone path,
a label another env file already claims, or a clone path outside the new user's
`$HOME` all abort with nothing written. That last one matters — two workers
sharing a working tree check out branches and hard-reset onto origin's default,
and one tree corrupts the other.

Three things it can't do for you:

1. **The label triad** — `$LABEL`, `$LABEL:planned`, `$LABEL:done` — must exist
   in every repo the instance serves. The worker moves those labels, it never
   creates them. Make them in GitHub's UI or with `gh` from your workstation.

2. **The subscription login**, as the person who holds it:

   ```bash
   sudo -u claudius-maximus -H bash -l
     npm install -g @anthropic-ai/claude-code   # >= 2.1.186, for RETRY_WATCHDOG
     claude login && claude doctor
     unset ANTHROPIC_API_KEY                    # and remove it from any profile
   ```

3. **The GitHub device flow.** Run the worker once in the foreground as that
   user; it prints a one-time code and URL to open as this instance's GitHub
   account, then Ctrl-C. The script prints the exact command. The token lands in
   `$HOME/.claudius-maximus/github-token` (mode `0600`), never in the env file —
   a file rather than the OS keyring because the instance is a `nologin` user
   under systemd, with no session for a keyring to live in.

That account needs write access to every repo in `$REPOS` (org member or
collaborator, `read:org` for org repos): the worker pushes `claude/*` branches
and opens PRs as this account. Claude itself never pushes — there are no git
credentials in the clone and no `gh` on the box.

Then start it:

```bash
systemctl enable --now claudius@claudius-maximus
journalctl -u claudius@claudius-maximus -f
```

On start the instance posts `:crown: awake` to Mattermost with the repos it will
drain, then `:scroll: planned ORG/REPO#N` / `:white_check_mark: shipped
ORG/REPO#N` / `:warning: ORG/REPO#N failed` per issue, all under its `$INSTANCE`
name, so several instances share a channel legibly.

### Updating the binary

```bash
cargo build --release          # target/release/claudius-maximus
scp target/release/claudius-maximus \
    <box>:/home/claudius-maximus/claudius-maximus/claudius-maximus
ssh <box> sudo systemctl restart claudius@claudius-maximus
```

`git2` is built with `vendored-libgit2`/`vendored-openssl`, so the result is one
binary with no system libgit2, no OpenSSL, and no `git`, `gh` or `curl` to find
at runtime. Build on a box matching the target architecture (or cross-compile);
the only other thing the server needs is Claude Code.

Migrating a box that still runs the bash worker: stop the unit, `scp` the binary
over `worker.sh`, complete the device flow once in the foreground, point
`ExecStart` at the binary, `daemon-reload && restart`. The env file, labels,
clones and in-flight issue state carry over unchanged; `worker.sh`, `repos.sh`
and the `gh` CLI can go.

## Config

`/etc/<user>.env`, one per instance, named after the unix user — so
`/etc/claudius-maximus.env` (chmod 640, owned by that user). `add-instance.sh`
writes it and derives `LABEL` and `INSTANCE` from the same name.

```bash
REPOS=foro-sh/claudius-maximus=/home/claudius-maximus/repos/claudius-maximus,foro-sh/foro=/home/claudius-maximus/repos/foro=danielsteman|thijssdaniels
GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx  # the OAuth App the device flow authorizes against
LABEL=claudius-maximus           # optional; defaults to claudius-maximus
INSTANCE=Claudius Maximus        # optional; name in logs + Mattermost
PLAN_MODEL=claude-opus-5         # optional; worker's plan step
PLAN_EFFORT=high                 # optional; low|medium|high|xhigh|max
IMPLEMENT_MODEL=claude-sonnet-5  # optional; worker's implement step
IMPLEMENT_EFFORT=high            # optional; low|medium|high|xhigh|max
POLL_INTERVAL=60                 # optional, seconds
STATUS_ADDR=127.0.0.1:9781       # optional; status page + /metrics + /healthz, 'off' to disable
HEARTBEAT_INTERVAL=60            # optional, seconds; 0 = no periodic heartbeat line
STALL_AFTER=1800                 # optional, seconds; how long a silent run runs before it is mentioned
CLAUDIUS_CLAIM_DIR=/tmp          # optional; where the $LABEL lock lives
CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx   # optional
# Commit attribution: must be a verified email on this instance's GitHub account.
GIT_AUTHOR_NAME="Daniel Steman"
GIT_AUTHOR_EMAIL=daniel-steman@live.nl
GIT_COMMITTER_NAME="Daniel Steman"
GIT_COMMITTER_EMAIL=daniel-steman@live.nl
```

`add-instance.sh` passes all of these through, so you can set any of them in the
`sudo env` line above.

`REPOS` is a comma-separated list of
`owner/name[=/abs/path/to/clone][=author|author]` entries, **no spaces**. Omit
the path and `add-instance.sh` expands it to
`/home/<instance>/repos/<owner>/<name>` and writes the absolute form (the worker
requires absolute paths, under that instance's own `$HOME`). A malformed entry
aborts the worker at startup. Sweep order follows list order.

`GITHUB_CLIENT_ID` is a client id, not a secret: device flow has no client
secret, and every instance shares one app — the device flow is what makes each a
different *account*. Create it once under Settings → Developer settings → OAuth
Apps with "Enable Device Flow" ticked.

The `GIT_*` identity lives here rather than in the unit because each instance
commits as its own account holder. Quote values with spaces; unquoted, systemd
drops everything after the space.

The worker runs Claude with `--dangerously-skip-permissions` for both planning
and implementation, so it never blocks on an interactive approval prompt.

## Watching it work

A plan run is minutes, an implement run can be an hour, and a run that meets a
spent usage window is hours. The sweep loop is blocked for all of that, so an
instance doing exactly what it should looks, from outside, like one that has
hung. Four surfaces answer "what is it doing right now".

### The journal

```
$ journalctl -u claudius@claudius-maximus -f
Claudius Maximus: rising: repos=foro-sh/foro label=claudius-maximus …
Claudius Maximus: systemd is listening; status lines go to `systemctl status`, watchdog every 180s
Claudius Maximus: status page on http://127.0.0.1:9781/
Claudius Maximus: foro-sh/foro#12: implementing
Claudius Maximus: implementing foro-sh/foro#12 (claude-sonnet-5) for 5m00s, quiet for 12s
Claudius Maximus: foro-sh/foro#12 claude: Claude AI usage limit reached|1763481600
Claudius Maximus: foro-sh/foro#12: usage window spent (reopens in 1h12m), the run waits for the reset rather than failing
Claudius Maximus: waiting out a spent usage window for 20m00s, reopens in 52m00s (was implementing foro-sh/foro#12 (claude-sonnet-5))
Claudius Maximus: foro-sh/foro#12: usage window reopened after 1h12m, carrying on
Claudius Maximus: foro-sh/foro#12: implementing run finished after 1h31m
Claudius Maximus: sweep done in 1h31m: 3 issue(s) seen, 0 planned, 1 shipped, 1 skipped, 1 resting, 0 failed
```

The **heartbeat** repeats while a run is in flight or a window is being waited
out, and stays quiet between sweeps (`HEARTBEAT_INTERVAL`; `0` turns it off).
`claude`'s **stderr** is forwarded as it arrives, which is where a run says it
is in trouble. And every sweep ends with **what it did**, so a drained backlog
and a stopped loop stop looking alike.

A run that writes nothing for `STALL_AFTER` (default 30 min) is mentioned once,
in the journal and in Mattermost. It is never killed: the worker can't tell a
run that is thinking hard from one that is gone, and an hour of thinking is
still an hour of work worth having.

### `systemctl status`

The unit is `Type=notify`, so the current line is attached to the service:

```
$ systemctl status claudius@claudius-maximus
● claudius@claudius-maximus.service - Claudius (claudius-maximus): …
     Active: active (running) since Mon 2026-09-14 09:12:41 UTC; 2 days ago
     Status: "implementing foro-sh/foro#12 (claude-sonnet-5) for 12m30s, quiet for 41s"
```

`WatchdogSec=180` goes with it. The heartbeat thread answers the watchdog
independently of the sweep loop — a long run and a spent window are allowed to
block for hours and must never be restarted for it. What the watchdog catches is
a process that is still there with nobody home. `HEARTBEAT_INTERVAL=0` silences
the per-minute journal line without silencing the pings; the one-off line about
a run gone quiet carries on until `STALL_AFTER=0` turns that off too.

### The status page

`STATUS_ADDR` (a bare port means loopback; `off` disables it) serves four GETs:

| Route          | What it is                                                                                                                              |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `/`            | Current stage, a spent window with a countdown, counters, the last line `claude` wrote, and the chronicle of what has happened           |
| `/status.json` | The same thing for scripts                                                                                                              |
| `/metrics`     | Prometheus                                                                                                                              |
| `/healthz`     | `200` while the worker is still moving, `503` when it has stopped                                                                        |

```bash
$ curl -s localhost:9781/status.json | jq '{line, window, counters}'
{
  "line": "waiting out a spent usage window for 20m00s, reopens in 52m00s (was implementing foro-sh/foro#12 (claude-sonnet-5))",
  "window": { "shut": true, "shut_for_seconds": 1200, "resets_at_epoch": 1763481600, "claude_said": null },
  "counters": { "sweeps": 41, "issues_seen": 96, "plans_posted": 9, "pulls_opened": 8,
                "claude_runs": 17, "claude_failures": 1, "issue_failures": 1, "repo_failures": 0,
                "windows_shut": 2, "claude_seconds": 38211, "window_seconds": 9160 }
}
```

```yaml
# prometheus.yml
scrape_configs:
  - job_name: claudius
    static_configs:
      - targets: ["127.0.0.1:9781", "127.0.0.1:9782"]   # maximus, secundus
```

Graph `claudius_usage_window_seconds_total` first: time spent waiting for a
reset is this project's throughput ceiling, measured instead of guessed.
`claudius_claude_seconds_total` is wall time inside `claude` — waits included,
since a run waiting out a window is still a run — so time actually spent working
is the difference between the two, and the ratio of window to difference is how
much a second instance would buy you.

`/healthz` is deliberately not a progress check. A nine-hour implement run is
healthy, and so is the five-hour window it may be waiting out inside. What
returns `503` is a worker with no run in flight that hasn't moved on to anything
in ten poll intervals (never less than ten minutes) — the one failure neither
the watchdog nor `ps` can see. It asks what the worker moved to last rather than
when a sweep last *finished*, because the sweep that would prove the loop is
turning is the one still running. The grace therefore assumes every step between
runs is shorter than it; the first clone of a very large repo is the one that may
not be, so raise `POLL_INTERVAL` on such a box or expect a `503` while it lands.

A shut window is not an excuse of its own: `claude` is what waits one out, so it
always comes with a run in flight. One left standing alone is a run that died
without saying the window reopened, and excusing it would hide a stopped worker
for as long as the CLI claimed the window would last.

`add-instance.sh` derives the port from the instance's ordinal — maximus 9781,
secundus 9782, up to centesimus on 9880. It refuses an instance whose port
another env file claims, and the worker refuses to start if the port is in use
rather than running unwatchable.

Keep it on loopback unless you put something in front of it. Nothing secret is
on the page (no tokens, no prompts), but issue numbers, repo names and whatever
`claude` last printed are — and the last of those came out of a run over an
issue body somebody else may have written.

### The usage window

The worker reads three things out of a run's **stderr**: that the window is
spent, that it has reopened, and that it is nearly spent. Stderr only, and not
as a detail — a plan goes to stdout, and a plan for an issue *about* usage
windows says "usage limit reached" in as many words, so this repo would park
itself on an imaginary reset the first time it planned its own backlog. Stdout
still counts as the run being alive.

A spent window becomes a journal line, a status change, one Mattermost message
(`:hourglass_flowing_sand:`) and a countdown on the page; reopening becomes the
matching `:crown:` line. Each is said once per window, however many times the
CLI repeats itself.

This is best-effort by design, and it's the only place in the worker that is.
The CLI's wording is not an API, so a release that rephrases the notice makes
the worker quieter, never wrong: `claude`'s own retry watchdog waits the window
out and resumes, and nothing here interferes. A run that comes back with an
answer is treated as proof the window is open again, whatever was recognised on
the way; a run that *failed* is not, since giving up on a spent window is the
likeliest way for one to fail.

## Adding an instance

One subscription's rolling 5h window is the ceiling for a single instance: it
drains serially, one issue per sweep. Past that, add instances — each a
*separate subscription*, held by a separate person, on a separate GitHub
account. There's no fixed number; the box runs as many as you have
subscriptions for.

The setup is [the same script](#setup), run again. What has to line up:

- **`LABEL` is unique per instance.** The single most important line in the
  file. Point two workers at one label and both plan the same issue, then both
  implement it and open competing PRs. Two guards stop that: `add-instance.sh`
  refuses a label another `/etc/claudius-*.env` claims, and each worker takes an
  OS lock on `/tmp/claudius-label-<label>.lock` before its first sweep, exiting
  with `label <label> is already being drained by <instance>` if a live worker
  holds it. Move the lock with `CLAUDIUS_CLAIM_DIR`. It's a same-box guard only
  — two workers on different machines still need disjoint labels.
- **The instance boundary is a unix user**, not an env var: `$HOME` scopes both
  the Claude subscription credentials and the GitHub token, so N subscriptions
  means N homes. Each needs its own `claude login`, by the person who holds that
  subscription, and its own device flow.
- **Each instance owns its label triad exclusively** — `$LABEL` plus the
  `:planned` and `:done` labels derived from it. The third instance reads and
  writes `claudius-tertius:planned` / `:done`.
- **Each GitHub account needs write access** to every repo in its own `REPOS`.
  The lists need not match, and usually shouldn't.
- **No two instances share a clone.** One working tree per instance per repo,
  under that instance's own `$HOME`.

Branch names can't collide: `claude/issue-N` comes from the issue number, and
each issue belongs to exactly one queue.

### Routing work between queues

Which label you apply decides which subscription implements the issue. That
split is deliberately manual — assign labels evenly and the queues stay
balanced; there is no automatic distribution and none is wanted.

Dependencies cross queues without special handling: the blocked check only asks
whether a blocker is still *open*, not which label it carries. A
`claudius-secundus` issue can be blocked by a `claudius-maximus` one and will
wait for it.

## Target-repo prerequisites

Every repo in `$REPOS` needs:

- A `CLAUDE.md` documenting branch convention, test/lint commands, and "do not
  merge, human review required". The worker tells Claude to follow it.
- Branch protection on the default branch: PR required; Claude only pushes
  `claude/*`. The worker reads that branch off origin's HEAD, so `trunk` or
  `master` needs no configuration.
- The label triad, per instance.
- An author allowlist in `$REPOS` if the repo is public.

Nothing else. The worker clones the repo itself on its first sweep, with its own
token, which is what makes a private repo work without a credential stored on
the box. An existing clone is reused as it stands and its `origin` **must be an
HTTPS URL** — the token is all the worker offers, and an SSH remote would ask
for a key it doesn't have. A `$REPOS` path that exists but holds no clone is
refused rather than cloned over.

## Tests

```bash
cargo test --workspace
./test-add-instance.sh      # add-instance.sh's config validation
```

The convention is faking the external boundary rather than mocking internals:
GitHub, git and the `claude` CLI are traits (`cm_github::GithubClient`,
`cm_git::GitOps`, `cm_worker::claude_cli::Claude`) the state machine is driven
through with fakes, so the suite exercises the real state machine without
touching GitHub or spending quota. A fake run can say what a real one says while
waiting out a spent window, so that reporting is tested without a five-hour
wait; what it renders (page, JSON, metrics, `/healthz`) are pure functions over
a snapshot of the register, tested without a socket.

CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo build --workspace` and `cargo test --workspace` on every PR.

## Known ceilings

- **Nothing enforces Conventional Commits locally.** Claude commits with the
  repo's own `git`, so a `commit-msg` hook fires if the repo has one, but the
  prompt is the only thing asking for a conforming message — the target repo's
  commitlint CI job is what actually catches a bad one, after the PR is open.
- **Serial within an instance, one issue per sweep.** Extra repos are visited in
  order, so a large first repo delays the ones after it; reorder `$REPOS` to
  change priority. Concurrency inside one instance is a non-goal.
- **Balancing is a human job by design.** Nothing redistributes work, so an
  instance idles when nobody labels for it.
- **No approval gate between plan and implement, by design.** Applying the
  trigger label is the only human action required. The plan comment isn't
  decoration, though: the implementing sweep reads it back off the issue and
  hands it to Claude along with the issue, since the box has no GitHub access of
  its own. Editing that comment before the next sweep is the one way to steer
  the implementation — only comments the instance's own account wrote are read,
  so nobody else can post a plan for it to follow. Leave the `:crown: Plan by …`
  line (or the HTML marker under it) in place when you edit: one of the two is
  how the next sweep finds the plan. Delete the comment and the issue is simply
  planned again. There is no way to plan an issue without also implementing it.
- **`--dangerously-skip-permissions` during implement**, acceptable on an
  isolated, unprivileged box; tighten with a `settings.json` allowlist otherwise.
- **Retry on failure is whole-issue, and backs off.** A failed implement re-runs
  and Claude is told to reuse the existing branch/PR. The wait doubles per
  consecutive failure on the same issue (one `$POLL_INTERVAL`, then two, up to
  64), because one permanently stuck issue would otherwise spend the quota the
  rest of the backlog needs. Any success resets it, and so does restarting the
  worker: the counters are in memory.
- **Mattermost messages are controlled text** (no issue titles), so nothing
  richer than a fixed line per transition is posted.
- **Usage-window detection is a best guess, and only ever a report.**
- **The systemd watchdog proves liveness, not progress.** `/healthz` is what
  notices a worker that has stopped moving.
- **The status page is a window onto a running process, not a history.** The
  register lives in memory, like the backoff counters and for the same reason:
  GitHub holds the state that matters. Scrape `/metrics` to keep history.
