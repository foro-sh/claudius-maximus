# CLAUDE.md / AGENTS.md

Guidance for AI coding agents working in this repository.

## What this is

Claudius Maximus is a long-running worker that continuously drains
GitHub-issue backlogs into PRs using Claude Code, plan-then-implement, with
GitHub labels as its only state (no DB, no queue). It started as a bash
script (`infra/claudius-maximus/`) inside a private monorepo and was split
out here to become its own project — see foro-sh/claudius-maximus#1 for the
current direction (a Rust rewrite: `octocrab` for the GitHub API via device-flow
OAuth, `git2` for git operations, `Command` reserved for the `claude` CLI
only, since neither `git`/`gh` nor Claude Code has a reason to be shelled out
to twice).

Not business-critical — this repo is a deliberately over-engineered, fun
project, not a runtime dependency of anything else.

## Breaking changes over fallbacks

Worth restating on a young project: no external consumers to keep stable yet,
so prefer clean breaking changes over backwards-compatibility scaffolding.

- Do **not** add fallbacks, compatibility shims, or dual code paths that keep
  an old and new way working "just in case." Pick the design and commit to it.
- When a change breaks something else, fix the caller or delete the dead
  behavior — don't preserve both.
- No silent `try`/`catch`-and-ignore (or Rust's equivalent, swallowing a
  `Result` with `.ok()` where the error mattered) that papers over a now-invalid
  state. Let it fail loudly.

## Commit discipline

Make atomic commits: each commit is one coherent, self-contained change.
Conventional Commits are enforced by `commitlint` via CI (`<type>(scope):
description`).

**Commitlint rules (config-conventional):**
- **Type** (required): lowercase, one of: `feat`, `fix`, `chore`, `ci`, `docs`,
  `style`, `refactor`, `perf`, `test`
- **Scope** (optional): wrap in parentheses, e.g. `feat(worker): add device-flow login`
- **Description** (required): start with lowercase, imperative mood, no
  trailing period, max ~72 chars
- **Body** (optional): separated from description by a blank line, wrapped at 72
  chars
- **Breaking changes:** mark with `BREAKING CHANGE: ` in the footer

If commitlint fails, fix the message and try again — do not bypass it.

**Versioning is automated.** semantic-release derives the version from commit
messages on every push to `main` — never hand-edit a version or
`CHANGELOG.md`. This repo starts at `0.0.0` (tag pushed before the first
release) and stays on the **0.x** line: a breaking change bumps the minor, not
the major (see `.releaserc.json`'s `commit-analyzer` override), so `1.0.0`
stays a deliberate promotion rather than something a `feat!:` commit triggers
by accident.

For how changes get integrated — commit everything, group into reviewed PRs
by concern, self-review and commit fixes, and **ask before merging** — follow
the `committing-and-pr-workflow` skill if it's available in your harness;
otherwise apply the same discipline by hand. Never merge a PR without explicit
human approval.
