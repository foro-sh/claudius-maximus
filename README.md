# Claudius Maximus

<p align="center">
  <img src="assets/claudius-maximus.jpg" alt="Claudius Maximus" width="100%">
</p>

A single long-running binary that continuously turns labeled GitHub issues into
PRs using a Claude Code **subscription** (OAuth, not `ANTHROPIC_API_KEY`),
plan-then-implement, to max out the rolling 5h usage window. No webhook, no
Tailscale Funnel, no queue daemon — a poll loop over the GitHub API with GitHub
labels as the only state. The worker only makes outbound calls, so it needs no
inbound tunnel. Status updates post to Mattermost if configured.

One **instance** = one subscription = one unix user, run from the
`claudius@.service` template. Beyond one subscription's rolling window,
throughput comes from adding instances — any number of them, each draining its
own label. `add-instance.sh` provisions one; see
[Adding an instance](#adding-an-instance).

It started as a bash script in `foro-sh/platform` (`infra/claudius-maximus/`)
and is being ported to Rust here: `octocrab` for the GitHub API over device-flow
OAuth, `git2` for git, and `std::process::Command` reserved for the `claude` CLI
alone — so the box needs neither the `gh` CLI nor a `git` binary. The port is in
progress under
[#1](https://github.com/foro-sh/claudius-maximus/issues/1); this README
describes the binary that issue specifies.

## How it works

| Issue state                                    | Worker action                                                                                            |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Opened by an author outside the repo's allowlist | Skipped outright, before planning — see "Who may file an issue CM acts on"                               |
| Blocked by an open issue (GitHub relationship)  | Skipped until every blocker closes                                                                        |
| Labeled `claudius-maximus`, not yet `:planned`  | Claude posts an implementation plan, adds `claudius-maximus:planned`                                      |
| `:planned`                                      | Claude implements and commits on `claude/issue-N`; the worker pushes it and opens the PR (`Closes #N`), adds `:done`, removes the trigger label |
| `claudius-maximus:done`                         | ignored                                                                                                   |

Quota exhaustion is handled by Claude (`CLAUDE_CODE_RETRY_WATCHDOG=1`): it waits
for the window to reset and resumes. A reboot loses nothing — all state is in
labels.

### Multiple repos

`REPOS` takes a list, and one worker serves all of it: a sweep walks the repos in
the configured order, drains each one's backlog, then sleeps. Adding a repo
widens what that one worker looks at — it does **not** start a second Claude
process, so an instance stays serial on its single subscription. (Adding an
*instance* is the thing that adds a process; see
[Adding an instance](#adding-an-instance).)

Each repo needs its own clone and its own copy of the three labels; the state
machine above then runs per repo, independently. Because issue numbers repeat
across repos, logs and Mattermost lines are qualified — `foro-sh/foro#12`, not
`#12`.

### Who may file an issue CM acts on

A `$REPOS` entry can end in a third field: a `|`-separated allowlist of issue
**authors**. Issues opened by anyone else are skipped outright — before planning,
before any `claude` call.

```
foro-sh/foro=/home/claudebot/repos/foro=danielsteman|thijssdaniels
```

A **public** repo needs this gate: without it, a stranger's issue would become a
Claude prompt the moment someone applied the label. A private repo can go
without one — filing already requires access. This is the only gate on who can
trigger an implementation: applying the trigger label is sufficient, there is
no separate approval step. Logins are matched case-insensitively.

## Build and deploy

```bash
cargo build --release          # target/release/claudius-maximus
scp target/release/claudius-maximus \
    <box>:/home/claudebot/claudius-maximus/claudius-maximus
ssh <box> sudo systemctl restart claudius@claudebot
```

`git2` is built with `vendored-libgit2`/`vendored-openssl`, so the result is one
binary with no system libgit2, no OpenSSL, and no `git`, `gh` or `curl` to find
at runtime. Build on a box matching the target's architecture (or cross-compile);
the only thing that has to exist on the server besides the binary is Claude Code.

## One-time server setup (run as `claudebot`)

This is the first instance, set up by hand. Every step here is **per
instance** — each further subscription repeats all of it as its own unix user,
with its own label triad, which is what `add-instance.sh` automates
([Adding an instance](#adding-an-instance)).

```bash
# 1. Claude Code >= 2.1.186 (RETRY_WATCHDOG), subscription login, no API key.
npm install -g @anthropic-ai/claude-code
claude login && claude doctor
unset ANTHROPIC_API_KEY            # and remove it from any profile/env

# 2. Clone every target repo (one clone per entry in $REPOS).
git clone https://github.com/foro-sh/platform.git /home/claudebot/repos/platform
git clone https://github.com/foro-sh/foro.git     /home/claudebot/repos/foro

# 3. Drop the binary in place.
mkdir -p /home/claudebot/claudius-maximus
# ...scp target/release/claudius-maximus here, then:
chmod +x /home/claudebot/claudius-maximus/claudius-maximus
```

**GitHub login is the binary's own job.** On first run it starts GitHub's OAuth
device flow: it prints a one-time code and a verification URL, you open the URL
once as the account this instance acts as, and the token is stored in
`$HOME/.claudius-maximus/github-token` (mode `0600`) — not in
`/etc/claudius-<user>.env`. A file rather than the OS keyring because the
instance is a `nologin` user under systemd, with no login session and no Secret
Service for a keyring to live in; `$HOME` already holds that instance's Claude
subscription credentials, so the GitHub token sits beside them. Same
one-time ceremony `gh auth login` used to be, with no `gh` CLI on the box at all.
Run it in the foreground once before enabling the unit, so you can complete the
flow. It validates its config before anything else, so write
[`/etc/claudius-claudebot.env`](#config) first and source it for this one run —
systemd reads it for you afterwards:

```bash
set -a; . /etc/claudius-claudebot.env; set +a
/home/claudebot/claudius-maximus/claudius-maximus     # prints code + URL
```

The account must have write access to every repo in `$REPOS` (org member or
collaborator, `read:org` for org repos): the worker pushes `claude/*` branches
with this token and opens the PRs as this account. Claude itself never pushes —
there are no git credentials in the clone for it to use, and no `gh` on the box.

The three labels (`$LABEL`, `:planned`, `:done`) must exist in every repo the
instance serves. Create them from GitHub's web UI or with `gh` from your own
workstation — the worker reads and moves them, it never creates them, and
nothing on the box needs `gh` for this.

The worker uses `--dangerously-skip-permissions` for both planning and
implementation so it can run unattended on a home server without blocking on
interactive approval prompts for GitHub/network access.

## Config

`/etc/claudius-<user>.env` — one per instance, named after the unix user the
instance runs as, so `/etc/claudius-claudebot.env` (chmod 640, owned by
`claudebot`):

```bash
REPOS=foro-sh/platform=/home/claudebot/repos/platform,foro-sh/foro=/home/claudebot/repos/foro=danielsteman|thijssdaniels
GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx  # the OAuth App the device flow authorizes against
LABEL=claudius-maximus           # optional; the queue this instance owns
INSTANCE=Claudius Maximus        # optional; name in logs + Mattermost
PLAN_MODEL=claude-opus-5         # optional; worker's plan step
PLAN_EFFORT=high                 # optional; low|medium|high|xhigh|max
IMPLEMENT_MODEL=claude-sonnet-5  # optional; worker's implement step
IMPLEMENT_EFFORT=high            # optional; low|medium|high|xhigh|max
POLL_INTERVAL=60                 # optional, seconds
CLAUDIUS_CLAIM_DIR=/tmp          # optional; where the $LABEL lock lives, see below
CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx   # optional
# Commit attribution — must be a verified email on this instance's GitHub account.
GIT_AUTHOR_NAME="Daniel Steman"
GIT_AUTHOR_EMAIL=daniel-steman@live.nl
GIT_COMMITTER_NAME="Daniel Steman"
GIT_COMMITTER_EMAIL=daniel-steman@live.nl
```

Same env-var surface as the bash worker, plus `GITHUB_CLIENT_ID` — the bash
worker leaned on `gh`'s own OAuth app, this one has no `gh` to borrow from. It is
a client id, not a secret: device flow has no client secret, and every instance
shares the one app (the device flow is what makes each a different *account*).
Create it once under Settings → Developer settings → OAuth Apps with "Enable
Device Flow" ticked. No GitHub token here: that lives in the instance's home.

The `GIT_*` identity lives here rather than in the unit because it differs per
instance — each instance commits as its own account holder. Quote values
containing spaces; unquoted, systemd drops everything after the space.

`REPOS` is a comma-separated list of `owner/name=/abs/path/to/clone` entries,
each with an optional `=author|author` allowlist — **no spaces**, and the path
must be absolute. A malformed entry aborts the worker at startup rather than
silently auditing the wrong tree. Sweep order follows list order.

Locally running Mattermost on the same box: use its
`http://localhost:8065/hooks/...` incoming-webhook URL.

## Run under systemd (as root)

`claudius@.service` is a template unit; the instance name is the **unix user** it
runs as, which is what scopes the subscription login, the GitHub device-flow
token, and the clones.

```bash
cp claudius@.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now claudius@claudebot
journalctl -u claudius@claudebot -f
```

On start, the instance posts ":crown: awake" to Mattermost (listing the repos it
will drain), then ":scroll: planned ORG/REPO#N" / ":white_check_mark: shipped
ORG/REPO#N" / ":warning: ORG/REPO#N failed" per issue — all under its `$INSTANCE`
name, so several instances in one channel stay tellable apart.

Migrating a box that runs the bash worker: stop the unit, `scp` the binary next
to (or over) `worker.sh`, complete the device-flow login once in the foreground,
point `ExecStart` at the binary, `systemctl daemon-reload && systemctl restart`.
The env file, labels, clones and in-flight issue state all carry over unchanged
— `worker.sh`, `repos.sh` and the `gh` CLI can then go.

## Adding an instance

One subscription's rolling 5h window is the throughput ceiling for a single
instance — it drains serially, one issue per sweep. Throughput past that comes
from adding instances: each one is a *separate subscription*, held by a separate
person, on a separate GitHub account. There is no fixed number of them. The box
runs as many as you have subscriptions for.

**The instance boundary is a unix user.** Not an env var: `$HOME` is what scopes
the Claude subscription OAuth credentials *and* the file the GitHub token
lands in, so N subscriptions means N homes. Nothing supervises them from inside
the binary — one process serves one subscription, and systemd runs the set. The
template unit takes the user as its instance name, so `claudius@claudebot`,
`claudius@claudebot2`, `claudius@claudebot3` are three independent units of the
same shape.

**Labels are the partition, and they must not overlap.** Point two workers at the
same label and both would plan the same issue, then both would implement it and
open competing PRs. The second one to start won't get that far: each worker takes
an OS lock on `/tmp/claudius-label-<label>.lock` before its first sweep and exits
with `label <label> is already being drained by <instance>` if a live worker
holds it — so the mistake shows up as a failed unit, seconds after `systemctl
start`, rather than as duplicate PRs. Point the lock somewhere other than `/tmp`
with `CLAUDIUS_CLAIM_DIR` (any directory every instance on the box can write). It
is a same-box guard only: two workers on different machines still need disjoint
labels, which is the config discipline below. Each instance owns its own label
triad exclusively — `$LABEL` plus the `:planned` and `:done` state labels the
worker derives from it, so setting `LABEL=claudius-tertius` is what makes it read
and write `claudius-tertius:planned` / `:done`.

An instance also only looks at the repos in **its own** `$REPOS`. The lists need
not match, and usually shouldn't all be the same.

Each instance must be logged in as the person who actually holds that
subscription — `claude login` on their own account, not a shared credential, and
its own device-flow login for GitHub.

### Setup

`add-instance.sh` does the mechanical half — the unix user, its clones, its env
file, the unit — and prints the rest. Run it as root from a checkout, once per
subscription:

```bash
cargo build --release
sudo env REPOS=foro-sh/platform=/home/claudebot3/repos/platform \
         GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx \
         GIT_AUTHOR_NAME="Person 3" \
         GIT_AUTHOR_EMAIL=<verified-email-on-person-3s-github-account> \
         CLAUDIUS_MAXIMUS_MATTERMOST_WEBHOOK_URL=http://localhost:8065/hooks/xxxx \
    ./add-instance.sh claudebot3 claudius-tertius "Claudius Tertius"
```

It validates the config before it creates anything: a `$REPOS` typo, a relative
clone path, a label another instance's env file already claims, or a clone path
outside the new user's `$HOME` all abort with nothing written. That last one is
not a style rule — two workers sharing a working tree both check out branches and
hard-reset onto origin's default, and one tree corrupts the other. Re-running is
safe; an existing user, clone or env file is left alone.

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
   sudo -u claudebot3 -H bash -l
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
systemctl enable --now claudius@claudebot3
journalctl -u claudius@claudebot3 -f
```

### What has to line up

- **`LABEL` is unique per instance.** The single most important line in the file.
  `add-instance.sh` refuses a label another `/etc/claudius-*.env` already claims,
  and the worker's startup lock catches the rest.
- **Each instance's GitHub account needs write access** to every repo in its
  `REPOS` (org member or collaborator) — the worker pushes `claude/*` branches
  and opens PRs with that account's token.
- **All three of its labels must exist** in every repo it serves, before it runs.
- **No two instances share a clone.** One working tree per instance per repo,
  under that instance's own `$HOME`.
- **Branch names can't collide.** `claude/issue-N` is derived from the issue
  number, and each issue belongs to exactly one queue, so two instances never
  target the same branch.

### Routing work between the queues

Which label you apply decides which subscription implements the issue. That split
is deliberately manual — assign labels evenly by hand and the queues stay
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
spending subscription quota. CI runs `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo build --workspace` and
`cargo test --workspace` on every PR.

## Target-repo prerequisites

Every repo in `$REPOS` needs all of these:

- A `CLAUDE.md` documenting branch convention, test/lint commands, and "do not
  merge — human review required". The worker tells Claude to follow it.
- Branch protection on `main`: PR required; Claude only pushes `claude/*`.
- The three labels (`$LABEL`, `:planned`, `:done`). **Per instance**: a second
  instance needs its own triad, since the label is what keeps the two queues
  from colliding.
- An author allowlist in `$REPOS` if the repo is public, so a stranger's issue
  can't become a Claude prompt.
- A clone on the box at the path given in `$REPOS`, with `main` checked out and
  an `origin` the bot can fetch. **Per instance** — two workers must never share
  a working tree. `add-instance.sh` clones over anonymous HTTPS, so a **private**
  repo has to be cloned by hand as that unix user, with credentials of your
  choosing — the worker's own token only arrives later, at the device flow.
  From then on the worker fetches and pushes with that token, so the clone needs
  no stored credential of its own.

## Known ceilings

- **Nothing enforces Conventional Commits locally.** Claude makes the commits
  with the repo's own `git`, so a `commit-msg` hook fires if the repo has one —
  but the prompt is the only thing that asks for a conforming message, and the
  target repo's commitlint CI job is what actually catches a bad one, after the
  PR is open.
- **Serial within an instance, one issue per sweep.** Extra repos are visited in
  order within a sweep, so a large first repo delays the ones after it — reorder
  `$REPOS` to change priority. Concurrency *inside* one instance is a non-goal;
  scale out with another instance instead.
- **Instances coordinate on exactly one thing: the label.** Each worker takes an
  OS lock on `/tmp/claudius-label-<label>.lock` at startup and refuses to run if
  another live worker holds it, so a duplicated `$LABEL` stops at the second unit
  instead of reaching GitHub as competing PRs. Nothing else is shared — no work
  distribution, no view of what the other is doing — and the guard is per box: two
  workers on *different* machines with the same label still collide.
- **Balancing is a human job by design.** Nothing redistributes work, so an
  instance idles when nobody labels for it. Assign labels evenly; that's the
  mechanism.
- **No approval gate between plan and implement, by design.** Applying the
  trigger label is the only human action required; nobody has to approve the
  plan. The plan comment is not decoration, though — the implementing sweep
  reads it back off the issue and hands it to Claude, together with the issue
  itself, since the box has no GitHub access of its own. Editing the plan
  comment before the next sweep is the one way to steer the implementation.
  There is currently no way to plan an issue without also implementing it.
- **`--dangerously-skip-permissions`** during implement — acceptable on an
  isolated, unprivileged box; tighten with a `settings.json` allowlist otherwise.
- **Retry on failure is whole-issue.** A failed implement re-runs next sweep;
  Claude is told to reuse the existing branch/PR rather than duplicate it.
- **Mattermost messages are controlled text** (no issue titles), so nothing
  richer than a fixed line per transition is posted.
