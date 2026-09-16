---
name: declaring-issue-dependencies
description: Use when ordering the backlog for Claudius Maximus or whenever issue execution order matters — declares GitHub "blocked by" relationships (the issue DAG) via gh api. Triggers on "draw the edges", "issue dependencies", "what order should CM pick up issues", "block issue X on Y", or labeling a batch of issues for claudius-maximus.
---

# Declaring issue dependencies

## Why

Claudius Maximus (`infra/claudius-maximus/worker.sh`) skips any issue whose
GitHub "blocked by" issues are still open, and re-checks every sweep. A blocker
closes when its PR merges (`Closes #N`), which unblocks its dependents — so the
declared edges ARE the execution order. No edges = plain issue-number order.

## What counts as an edge

Declare `B blocked by A` only for **hard** dependencies:

- B builds on code/schema/API/config that A introduces.
- B and A rewrite the same files and merging both independently would conflict.

Do NOT declare edges for priority, milestones, or "feels like it should come
first" — soft ordering. Every edge serializes work and one stuck blocker
starves its whole subtree. When in doubt, leave the edge out.

## Workflow

Declare edges in whichever repo the blocked issue lives in. CM drains every repo
in its `$REPOS` list, so set `REPO=owner/name` before running the commands below. `blocked()` treats *any*
open entry the API returns for that issue as blocking, so whatever GitHub lists
there is honoured.

1. **List real issues** (never the raw issues API — it mixes in PRs):

   ```bash
   gh issue list --repo $REPO --state open --json number,title,body,milestone
   ```

2. **Infer candidate edges** from titles/bodies/touched areas, then **show the
   proposed edge list to the user and get a yes before writing** — edges are
   repo mutations and steer the bot.

3. **Create each edge.** The API wants the blocker's *database id* (`.id`),
   not its issue number:

   ```bash
   blocker_id=$(gh api repos/$REPO/issues/<BLOCKER_NUM> -q .id)
   gh api -X POST "repos/$REPO/issues/<BLOCKED_NUM>/dependencies/blocked_by" \
     -F issue_id="$blocker_id"
   ```

   A 422 "may only be an issue" means one of the numbers is a PR.

4. **Verify** by reading back:

   ```bash
   gh api "repos/$REPO/issues/<BLOCKED_NUM>/dependencies/blocked_by" \
     -q '.[] | "\(.number) \(.state)"'
   ```

5. **Undo a wrong edge:**

   ```bash
   gh api -X DELETE "repos/$REPO/issues/<BLOCKED_NUM>/dependencies/blocked_by/$blocker_id"
   ```

## Sanity checks after drawing

- **No cycles**: a cycle means CM never picks any issue in it. After drawing,
  print each issue's open blockers (step 4 per issue) and eyeball that the
  graph flows one way. GitHub's UI (issue → Relationships) shows the same.
- **Blockers are doable**: every blocker should itself be labeled for CM (or
  assigned to a human who knows it's gating things) — an unlabeled blocker
  silently stalls its subtree.
