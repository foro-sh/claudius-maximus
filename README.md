# Claudius Maximus

<p align="center">
  <img src="assets/claudius-maximus.jpg" alt="Claudius Maximus" width="100%">
</p>

A single long-running binary that continuously turns labeled GitHub issues into
PRs using a Claude Code **subscription** (OAuth, not `ANTHROPIC_API_KEY`),
plan-then-implement, to max out the rolling 5h usage window. No webhook, no
Tailscale Funnel, no queue daemon: a poll loop over the GitHub API with GitHub
labels as the only state. The worker only makes outbound calls, so it needs no
inbound tunnel. Status updates post to Mattermost if configured, and it says
what it is doing while it does it: a heartbeat in the journal, a live line
under `systemctl status`, and a status page with `/metrics` and `/healthz`
beside it (see [Watching it work](#watching-it-work)).

One **instance** = one subscription = one unix user, run from the
`claudius@.service` template. Beyond one subscription's rolling window,
throughput comes from adding instances (any number of them, each draining its
own label). `add-instance.sh` provisions one; see
[Adding an instance](#adding-an-instance).

It started as a bash script (`infra/claudius-maximus/`) in a private monorepo
and is being ported to Rust here: `octocrab` for the GitHub API over device-flow
OAuth, `git2` for git, and `std::process::Command` reserved for the `claude` CLI
alone, so the box needs neither the `gh` CLI nor a `git` binary. The port is in
progress under
[#1](https://github.com/foro-sh/claudius-maximus/issues/1); this README
describes the binary that issue specifies.

## How it works

| Issue state                                    | Worker action                                                                                            |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Opened by an author outside the repo's allowlist | Skipped outright, before planning; see "Who may file an issue CM acts on"                               |
| Blocked by an open issue (GitHub relationship)  | Skipped until every blocker closes                                                                        |
| Labeled `claudius-maximus`, not yet `:planned`  | Claude posts an implementation plan, adds `claudius-maximus:planned`                                      |
| `:planned`                                      | Claude implements and commits on `claude/issue-N`; the worker pushes it and opens the PR (`Closes #N`), adds `:done`, removes the trigger label |
| `claudius-maximus:done`                         | ignored                                                                                                   |

Quota exhaustion is handled by Claude (`CLAUDE_CODE_RETRY_WATCHDOG=1`): it waits
for the window to reset and resumes the run it was in the middle of, so a spent
window costs the backlog time and nothing else. The worker does not manage that
wait, it only reports it: it reads the limit notice out of the run's output and
says, in the journal, in Mattermost and on the status page, that it is waiting
and for how long. See [the usage window](#the-usage-window). A reboot loses
nothing: all state is in labels.

### Multiple repos

`REPOS` takes a list, and one worker serves all of it: a sweep walks the repos in
the configured order, drains each one's backlog, then sleeps. Adding a repo
widens what that one worker looks at. It does **not** start a second Claude
process, so an instance stays serial on its single subscription. (Adding an
*instance* is the thing that adds a process; see
[Adding an instance](#adding-an-instance).)

Each repo needs its own clone and its own copy of the three labels; the state
machine above then runs per repo, independently. Because issue numbers repeat
across repos, logs and Mattermost lines are qualified: `foro-sh/foro#12`, not
`#12`.

### Who may file an issue CM acts on

A `$REPOS` entry can end in a third field: a `|`-separated allowlist of issue
**authors**. Issues opened by anyone else are skipped outright, before planning,
before any `claude` call.

```
foro-sh/foro=/home/claudius-maximus/repos/foro=danielsteman|thijssdaniels
```

A **public** repo needs this gate: without it, a stranger's issue would become a
Claude prompt the moment someone applied the label. A private repo can go
without one: filing already requires access. This is the only gate on who can
trigger an implementation: applying the trigger label is sufficient, there is
no separate approval step. Logins are matched case-insensitively.

## Build and deploy

```bash
cargo build --release          # target/release/claudius-maximus
scp target/release/claudius-maximus \
    <box>:/home/claudius-maximus/claudius-maximus/claudius-maximus
ssh <box> sudo systemctl restart claudius@claudius-maximus
```

`git2` is built with `vendored-libgit2`/`vendored-openssl`, so the result is one
binary with no system libgit2, no OpenSSL, and no `git`, `gh` or `curl` to find
at runtime. Build on a box matching the target's architecture (or cross-compile);
the only thing that has to exist on the server besides the binary is Claude Code.

## One-time server setup (run as `claudius-maximus`)

This is the first instance, set up by hand. Every step here is **per
instance**: each further subscription repeats all of it as its own unix user,
with its own label triad, which is what `add-instance.sh` automates
([Adding an instance](#adding-an-instance)). Prefer that script even for the
first slot: it assigns `claudius-maximus` and wires user, label, and display
name together.

```bash
# 1. Claude Code >= 2.1.186 (RETRY_WATCHDOG), subscription login, no API key.
npm install -g @anthropic-ai/claude-code
claude login && claude doctor
unset ANTHROPIC_API_KEY            # and remove it from any profile/env

# 2. Drop the binary in place. (The clones are the worker's own job, it
#    makes them on its first sweep, with its own token.)
mkdir -p /home/claudius-maximus/claudius-maximus
cd /home/claudius-maximus/claudius-maximus
curl -fsSLo claudius-maximus \
  https://github.com/foro-sh/claudius-maximus/releases/latest/download/claudius-maximus-linux-x86_64
chmod +x claudius-maximus     # or scp your own target/release/claudius-maximus here
```

**GitHub login is the binary's own job.** On first run it starts GitHub's OAuth
device flow: it prints a one-time code and a verification URL, you open the URL
once as the account this instance acts as, and the token is stored in
`$HOME/.claudius-maximus/github-token` (mode `0600`), not in
`/etc/claudius-<user>.env`. A file rather than the OS keyring because the
instance is a `nologin` user under systemd, with no login session and no Secret
Service for a keyring to live in; `$HOME` already holds that instance's Claude
subscription credentials, so the GitHub token sits beside them. Same
one-time ceremony `gh auth login` used to be, with no `gh` CLI on the box at all.
Run it in the foreground once before enabling the unit, so you can complete the
flow. It validates its config before anything else, so write
[`/etc/claudius-maximus.env`](#config) first and source it for this one
run; systemd reads it for you afterwards:

```bash
set -a; . /etc/claudius-maximus.env; set +a
/home/claudius-maximus/claudius-maximus/claudius-maximus     # prints code + URL
```

The account must have write access to every repo in `$REPOS` (org member or
collaborator, `read:org` for org repos): the worker pushes `claude/*` branches
with this token and opens the PRs as this account. Claude itself never pushes:
there are no git credentials in the clone for it to use, and no `gh` on the box.

The three labels (`$LABEL`, `:planned`, `:done`) must exist in every repo the
instance serves. Create them from GitHub's web UI or with `gh` from your own
workstation. The worker reads and moves them, it never creates them, and
nothing on the box needs `gh` for this.

The worker uses `--dangerously-skip-permissions` for both planning and
implementation so it can run unattended on a home server without blocking on
interactive approval prompts for GitHub/network access.

## Config

`/etc/<user>.env`, one per instance, named after the unix user the instance
runs as, so `/etc/claudius-maximus.env` (chmod 640, owned by `claudius-maximus`).
`add-instance.sh` sets `LABEL` and `INSTANCE` from that same name; you should
not invent a separate queue name.

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
HEARTBEAT_INTERVAL=60            # optional, seconds; 0 = no periodic heartbeat line in the journal
STALL_AFTER=1800                 # optional, seconds; how long a silent run runs before it is mentioned
CLAUDIUS_CLAIM_DIR=/tmp          # optional; where the $LABEL lock lives, see below
CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx   # optional
# Commit attribution: must be a verified email on this instance's GitHub account.
GIT_AUTHOR_NAME="Daniel Steman"
GIT_AUTHOR_EMAIL=daniel-steman@live.nl
GIT_COMMITTER_NAME="Daniel Steman"
GIT_COMMITTER_EMAIL=daniel-steman@live.nl
```

Same env-var surface as the bash worker, plus `GITHUB_CLIENT_ID`. The bash
worker leaned on `gh`'s own OAuth app, this one has no `gh` to borrow from. It is
a client id, not a secret: device flow has no client secret, and every instance
shares the one app (the device flow is what makes each a different *account*).
Create it once under Settings → Developer settings → OAuth Apps with "Enable
Device Flow" ticked. No GitHub token here: that lives in the instance's home.

The `GIT_*` identity lives here rather than in the unit because it differs per
instance: each instance commits as its own account holder. Quote values
containing spaces; unquoted, systemd drops everything after the space.

`REPOS` is a comma-separated list of
`owner/name[=/abs/path/to/clone][=author|author]` entries, **no spaces**. When
the path is omitted, `add-instance.sh` expands it to
`/home/<instance>/repos/<owner>/<name>` and writes the absolute form into the env file
(the worker still requires absolute paths). A malformed entry aborts the worker
at startup rather than silently auditing the wrong tree. Sweep order follows
list order.

Locally running Mattermost on the same box: use its
`http://localhost:8065/hooks/...` incoming-webhook URL.

## Run under systemd (as root)

`claudius@.service` is a template unit; the instance name is the **unix user** it
runs as, which is what scopes the subscription login, the GitHub device-flow
token, and the clones.

```bash
cp claudius@.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now claudius@claudius-maximus
journalctl -u claudius@claudius-maximus -f
```

On start, the instance posts ":crown: awake" to Mattermost (listing the repos it
will drain), then ":scroll: planned ORG/REPO#N" / ":white_check_mark: shipped
ORG/REPO#N" / ":warning: ORG/REPO#N failed" per issue, all under its `$INSTANCE`
name, so several instances in one channel stay tellable apart.

Migrating a box that runs the bash worker: stop the unit, `scp` the binary next
to (or over) `worker.sh`, complete the device-flow login once in the foreground,
point `ExecStart` at the binary, `systemctl daemon-reload && systemctl restart`.
The env file, labels, clones and in-flight issue state all carry over unchanged
`worker.sh`, `repos.sh` and the `gh` CLI can then go.

## Watching it work

A plan run is minutes, an implement run can be an hour, and a run that meets a
spent usage window is hours. The sweep loop is blocked for all of that, so an
instance doing exactly what it should looks, from outside, like one that has
hung. Four surfaces answer "what is it doing right now", in increasing order of
how much you have to have set up:

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

Three of those lines are new in kind rather than in wording. The **heartbeat**
repeats while a run is in flight or a window is being waited out, and stays
quiet between sweeps (`HEARTBEAT_INTERVAL`, seconds; `0` turns that line off).
`claude`'s **stderr** is forwarded as it arrives, which is where a run says it
is in trouble. And every sweep ends with **what the sweep did**, so a drained
backlog and a stopped loop stop looking alike.

A run that writes nothing for `STALL_AFTER` (default 30 minutes) is mentioned,
once, in the journal and in Mattermost. It is never killed: the worker cannot
tell a run that is thinking hard from one that is gone, and an hour of thinking
is still an hour of work worth having.

### `systemctl status`

The unit is `Type=notify`, so the current line is attached to the service:

```
$ systemctl status claudius@claudius-maximus
● claudius@claudius-maximus.service - Claudius (claudius-maximus): …
     Active: active (running) since Mon 2026-09-14 09:12:41 UTC; 2 days ago
     Status: "implementing foro-sh/foro#12 (claude-sonnet-5) for 12m30s, quiet for 41s"
```

`WatchdogSec=180` goes with it. The heartbeat thread answers it independently of
the sweep loop, which is the point: a long run and a spent window are allowed to
block for hours and must never be restarted for it. What the watchdog catches is
the process that is still there with nobody home. `HEARTBEAT_INTERVAL=0`
silences the per-minute line without silencing the pings; the one-off line
about a run that has gone quiet is not the pulse and carries on, until
`STALL_AFTER=0` turns that off too.

### The status page

`STATUS_ADDR` (a bare port means loopback; `off` disables it) serves four GETs:

| Route          | What it is                                                                 |
| -------------- | -------------------------------------------------------------------------- |
| `/`            | The page: current stage, a spent window with a countdown, counters, the last line `claude` wrote, and the chronicle of what has happened |
| `/status.json` | The same thing for scripts                                                  |
| `/metrics`     | Prometheus                                                                  |
| `/healthz`     | `200` while the worker is still moving, `503` when it has stopped           |

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

The metric worth putting on a graph first is
`claudius_usage_window_seconds_total`: the time this subscription spent waiting
for a reset is the throughput ceiling of the whole project, measured instead of
guessed. `claudius_claude_seconds_total` next to it is the time it spent
working, and the ratio is how much a second instance would buy you.

```yaml
# prometheus.yml
scrape_configs:
  - job_name: claudius
    static_configs:
      - targets: ["127.0.0.1:9781", "127.0.0.1:9782"]   # maximus, secundus
```

`/healthz` is deliberately not a progress check. A nine-hour implement run is
healthy, and so is the five-hour window it may be waiting out inside; what
returns `503` is a worker with no run in flight that has not moved on to
anything in ten poll intervals (never less than ten minutes), which is the one
failure neither the watchdog nor `ps` can see.

It asks what the worker moved to last rather than when a sweep last *finished*,
because a sweep over a full backlog is hours of short activities between long
runs - a PR pushed, labels moved, the next clone synced - and the sweep that
would prove the loop is turning is the one still running. A spent window is not
an excuse of its own and needs none: `claude` is what waits one out, so a shut
window comes with a run in flight. One left standing on its own is a run that
died without saying the window reopened, and excusing it would hide a stopped
worker for as long as the CLI said the window would last.

`add-instance.sh` derives the port from the instance's ordinal, so
claudius-maximus gets 9781, claudius-secundus 9782, and so on to
claudius-centesimus on 9880. It refuses to provision an instance whose port
another env file already claims, and the worker refuses to start if the port is
in use, rather than running unwatchable.

Keep it on loopback unless you put something in front of it. Nothing secret is
on the page (no tokens, no prompts), but issue numbers, repo names and whatever
`claude` last printed are, and the last of those came out of a run over an
issue body somebody else may have written.

### The usage window

The worker reads three things out of a run's output: that the window is spent,
that it has reopened, and that it is nearly spent. A spent window turns into a
journal line, a status change, one Mattermost message
(`:hourglass_flowing_sand:`) and a countdown on the page; reopening turns into
the matching `:crown:` line. Each is said once per window, however many times
the CLI repeats itself while it waits.

This is best-effort by design, and it is the only place in the worker that is.
The CLI's wording is not an API, so a release that rephrases the notice makes
the worker quieter, never wrong: `claude`'s own retry watchdog is what waits the
window out and resumes, and nothing here interferes with it. A run that comes
back with an answer is treated as proof the window is open again, whatever was
or was not recognised on the way; a run that *failed* is not, since giving up on
a spent window is the likeliest way for one to fail and announcing a recovery on
every retry would be worse than saying nothing.

## Adding an instance

One subscription's rolling 5h window is the throughput ceiling for a single
instance: it drains serially, one issue per sweep. Throughput past that comes
from adding instances: each one is a *separate subscription*, held by a separate
person, on a separate GitHub account. There is no fixed number of them. The box
runs as many as you have subscriptions for.

**The instance boundary is a unix user.** Not an env var: `$HOME` is what scopes
the Claude subscription OAuth credentials *and* the file the GitHub token
lands in, so N subscriptions means N homes. Nothing supervises them from inside
the binary: one process serves one subscription, and systemd runs the set.
`add-instance.sh` picks the unix user from a fixed Latin-ordinal list
(`claudius-maximus`, `claudius-secundus`, … `claudius-centesimus`, cap 100) and
uses that same string as `$LABEL` and, title-cased, as `$INSTANCE`, so you
never invent or align three names by hand. The template unit takes that user as
its instance name: `claudius@claudius-maximus`, `claudius@claudius-secundus`,
`claudius@claudius-tertius`.

**Labels are the partition, and they must not overlap.** Point two workers at the
same label and both would plan the same issue, then both would implement it and
open competing PRs. The second one to start won't get that far: each worker takes
an OS lock on `/tmp/claudius-label-<label>.lock` before its first sweep and exits
with `label <label> is already being drained by <instance>` if a live worker
holds it, so the mistake shows up as a failed unit, seconds after `systemctl
start`, rather than as duplicate PRs. Point the lock somewhere other than `/tmp`
with `CLAUDIUS_CLAIM_DIR` (any directory every instance on the box can write). It
is a same-box guard only: two workers on different machines still need disjoint
labels, which is the config discipline below. Each instance owns its own label
triad exclusively: `$LABEL` plus the `:planned` and `:done` state labels the
worker derives from it, so the third instance reads and writes
`claudius-tertius:planned` / `:done`.

An instance also only looks at the repos in **its own** `$REPOS`. The lists need
not match, and usually shouldn't all be the same.

Each instance must be logged in as the person who actually holds that
subscription: `claude login` on their own account, not a shared credential, and
its own device-flow login for GitHub.

### Setup

`add-instance.sh` does the mechanical half (the unix user, its env file, the
unit) and prints the rest. Run it as root from a checkout, once per
subscription. With no name argument it takes the next free slot from the ordinal
list; pass a known name explicitly only to re-run / repair that slot.

```bash
# Every release carries a static linux/x86_64 binary; grab that instead of
# building if the box has no Rust toolchain:
#   curl -fsSLo claudius-maximus https://github.com/foro-sh/claudius-maximus/releases/latest/download/claudius-maximus-linux-x86_64
#   chmod +x claudius-maximus && export CLAUDIUS_BINARY=$PWD/claudius-maximus
cargo build --release
sudo env REPOS=foro-sh/claudius-maximus \
         GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx \
         GIT_AUTHOR_NAME="Person 3" \
         GIT_AUTHOR_EMAIL=<verified-email-on-person-3s-github-account> \
         CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx \
    ./add-instance.sh
# → provisions claudius-maximus, then claudius-secundus, …
#    (or pass e.g. claudius-tertius to re-run that slot)
```

`REPOS` may be just `owner/name` (clone lands at
`/home/<instance>/repos/<owner>/<name>`), `owner/name=alice|bob` (same, with an author
allowlist), or the full `owner/name=/abs/path[=authors]` form. Absolute paths
must stay under the instance's own `$HOME`.

It validates the config before it creates anything: a `$REPOS` typo, a relative
clone path, a label another instance's env file already claims, or a clone path
outside the new user's `$HOME` all abort with nothing written. That last one is
not a style rule: two workers sharing a working tree both check out branches and
hard-reset onto origin's default, and one tree corrupts the other. Re-running a
named slot is safe; an existing user or env file is left alone. A plain re-run
with no args provisions the *next* free name.

`PLAN_MODEL`, `PLAN_EFFORT`, `IMPLEMENT_MODEL`, `IMPLEMENT_EFFORT`,
`POLL_INTERVAL`, `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` are passed
through the same way and otherwise take the defaults from [Config](#config).
`CLAUDIUS_BINARY` overrides where the binary is copied from.

Three things the script deliberately leaves to you, because none of them can be
automated:

1. **The label triad** (`$LABEL`, `$LABEL:planned`, `$LABEL:done`) must exist in
   every repo the instance serves, before it runs. The worker moves those labels;
   it never creates them. Create them from GitHub's web UI or with `gh` from your
   own workstation.

2. **The subscription login**, as the person who holds it:

   ```bash
   sudo -u claudius-tertius -H bash -l
     npm install -g @anthropic-ai/claude-code   # or a per-user install under ~/.local
     claude login && claude doctor              # person 3's own subscription
     unset ANTHROPIC_API_KEY
   ```

3. **The GitHub device flow.** Run the worker once in the foreground as that user,
   open the printed code and URL as this instance's GitHub account, then Ctrl-C.
   The script prints the exact command; the token lands in
   `$HOME/.claudius-maximus/github-token`, never in the env file.

Then start it:

```bash
systemctl enable --now claudius@claudius-tertius
journalctl -u claudius@claudius-tertius -f
```

### What has to line up

- **`LABEL` is unique per instance.** The single most important line in the file.
  `add-instance.sh` refuses a label another `/etc/claudius-*.env` already claims,
  and the worker's startup lock catches the rest.
- **Each instance's GitHub account needs write access** to every repo in its
  `REPOS` (org member or collaborator); the worker pushes `claude/*` branches
  and opens PRs with that account's token.
- **All three of its labels must exist** in every repo it serves, before it runs.
- **No two instances share a clone.** One working tree per instance per repo,
  under that instance's own `$HOME`.
- **Branch names can't collide.** `claude/issue-N` is derived from the issue
  number, and each issue belongs to exactly one queue, so two instances never
  target the same branch.

### Routing work between the queues

Which label you apply decides which subscription implements the issue. That split
is deliberately manual: assign labels evenly by hand and the queues stay
balanced; there is no automatic distribution and none is wanted.

Dependencies work across queues without any special handling: the blocked check
only asks whether a blocker issue is still *open*, not which label it carries. So
a `claudius-secundus` issue can be blocked by a `claudius-maximus` one and will
wait for it.

## Tests

```bash
cargo test --workspace
./test-add-instance.sh      # add-instance.sh's config validation
```

The convention is faking the external boundary rather than mocking internals: the
GitHub and git surfaces are traits (`cm_github::GithubClient`, `cm_git::GitOps`)
the state machine is driven through with fakes, and so is the `claude` CLI
(`cm_worker::claude_cli::Claude`), so the suite exercises the real state machine without touching GitHub or
spending subscription quota. A fake run can also say what a real one says while
it waits out a spent usage window, so the reporting in
[Watching it work](#watching-it-work) is tested without a five-hour wait; what
that reporting renders (the page, the JSON, the metrics, `/healthz`) are pure
functions over a snapshot of the register, tested without a socket. CI runs `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo build --workspace` and
`cargo test --workspace` on every PR.

## Target-repo prerequisites

Every repo in `$REPOS` needs all of these:

- A `CLAUDE.md` documenting branch convention, test/lint commands, and "do not
  merge, human review required". The worker tells Claude to follow it.
- Branch protection on the default branch: PR required; Claude only pushes
  `claude/*`. The worker reads that branch off origin's HEAD, so a repo on
  `trunk` or `master` needs no configuration.
- The three labels (`$LABEL`, `:planned`, `:done`). **Per instance**: a second
  instance needs its own triad, since the label is what keeps the two queues
  from colliding.
- An author allowlist in `$REPOS` if the repo is public, so a stranger's issue
  can't become a Claude prompt.
- Nothing else. The worker clones the repo itself on its first sweep, at the
  path given in `$REPOS`, using its own token, which is what makes a private
  repo work without a credential stored on the box. **Per instance**: two
  workers must never share a working tree, and a `$REPOS` path that exists but
  holds no clone is refused rather than cloned over. An existing clone is
  reused as it stands, and its `origin` **must be an HTTPS URL**: the token is
  all the worker offers, and an SSH remote would ask it for a key it does not
  have.

## Known ceilings

- **Nothing enforces Conventional Commits locally.** Claude makes the commits
  with the repo's own `git`, so a `commit-msg` hook fires if the repo has one,
  but the prompt is the only thing that asks for a conforming message, and the
  target repo's commitlint CI job is what actually catches a bad one, after the
  PR is open.
- **Serial within an instance, one issue per sweep.** Extra repos are visited in
  order within a sweep, so a large first repo delays the ones after it. Reorder
  `$REPOS` to change priority. Concurrency *inside* one instance is a non-goal;
  scale out with another instance instead.
- **Instances coordinate on exactly one thing: the label.** Each worker takes an
  OS lock on `/tmp/claudius-label-<label>.lock` at startup and refuses to run if
  another live worker holds it, so a duplicated `$LABEL` stops at the second unit
  instead of reaching GitHub as competing PRs. Nothing else is shared (no work
  distribution, no view of what the other is doing), and the guard is per box: two
  workers on *different* machines with the same label still collide.
- **Balancing is a human job by design.** Nothing redistributes work, so an
  instance idles when nobody labels for it. Assign labels evenly; that's the
  mechanism.
- **No approval gate between plan and implement, by design.** Applying the
  trigger label is the only human action required; nobody has to approve the
  plan. The plan comment is not decoration, though: the implementing sweep
  reads it back off the issue and hands it to Claude, together with the issue
  itself, since the box has no GitHub access of its own. Editing the plan
  comment before the next sweep is the one way to steer the implementation;
  only comments the instance's own GitHub account wrote are read, so nobody
  else can post a plan for it to follow. Leave the `:crown: Plan by …` line (or
  the HTML marker under it) in place when you edit: one of the two is how the
  next sweep finds the plan again. Delete the comment and the issue is simply
  planned again. There is currently no way to plan an issue without
  also implementing it.
- **`--dangerously-skip-permissions`** during implement, acceptable on an
  isolated, unprivileged box; tighten with a `settings.json` allowlist otherwise.
- **Retry on failure is whole-issue, and backs off.** A failed implement
  re-runs; Claude is told to reuse the existing branch/PR rather than duplicate
  it. The wait doubles per consecutive failure on the same issue (one
  `$POLL_INTERVAL`, then two, up to 64) because the retry is a whole Claude
  run and one permanently stuck issue would otherwise spend the quota the rest
  of the backlog needs. Any success on that issue resets it, and so does
  restarting the worker: the counters are in memory, GitHub holds the state
  that matters.
- **Mattermost messages are controlled text** (no issue titles), so nothing
  richer than a fixed line per transition is posted.
- **Usage-window detection is a best guess, and only ever a report.** It reads
  the `claude` CLI's own wording, which is not an API. A rephrased notice makes
  the worker quieter, never wrong: the CLI waits the window out and resumes
  either way, and a run that comes back is proof enough that the window
  reopened. See [the usage window](#the-usage-window).
- **The systemd watchdog proves liveness, not progress.** The heartbeat thread
  answers it independently of the sweep loop, because a long run and a spent
  window legitimately block for hours. A worker that has stopped moving is what
  `/healthz` is for.
- **The status page is a window onto a running process, not a history.** The
  register lives in memory, like the backoff counters and for the same reason:
  GitHub holds the state that matters, so a restart starts the counters over.
  Scrape `/metrics` if you want the history kept.
