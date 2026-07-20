---
name: delegating-with-kmux
description: >-
  Use only when the user explicitly requests delegating a defined implementation
  plan scope or task into a new kmux workspace.
---

## Trigger boundary

Use this skill only after the user explicitly asks to delegate work into a new
workspace. Do not infer delegation from a large task, an implementation plan, an
available launcher, or an opportunity to parallelize.

The launcher name `agent` is a user configuration convention required by this
workflow. It is not built into kmux and does not imply a particular agent runtime.

## Require a complete handoff

Identify all of the following before invoking kmux:

- the exact active implementation-plan path;
- the new target branch;
- the exact phases or task scope to delegate;
- the intended parent branch when it is not the current branch.

Ask the user when any value is materially ambiguous. Keep the delegated prompt
concise: name the repo-relative plan path and exact requested scope instead of
embedding the complete plan.

## Validate the source workspace

Before delegation:

1. Confirm the current directory belongs to the intended Git repository and note
   the current branch.
2. Confirm the plan file exists and is an active plan rather than a concluded or
   historical artifact.
3. Confirm the child worktree will receive the plan. It must either be tracked in
   the delegated commit history or covered by configured kmux copy/symlink
   behavior. Ask instead of assuming when this cannot be proven.
4. Inspect Git status. Stop when uncommitted source-workspace changes are required
   by the delegated task because a new worktree will not inherit them.
5. Check the proposed branch against local branches, remote branches, Git
   worktrees, existing kmux workspaces, workspace slugs, and tmux window naming.
6. Confirm that the selected parent relationship matches the plan and current
   branch topology.
7. Confirm a launcher named `agent` is present in the validated kmux config.
8. Confirm the selected tmux server has exactly one session containing live
   non-sidebar panes for the Git project and that session contains no panes from
   another Git project. Neutral non-Git panes do not affect the project bucket.
   Stop when the project is split across sessions or its session mixes projects.

Do not mutate or commit source-workspace changes merely to make delegation
possible unless the user separately requests that work.

## Delegate exactly once

Prefer `--input -` for multiline or sensitive handoff text so the prompt is not
placed in the original `kmux` process argv. Invoke one command with this shape:

```sh
kmux add <BRANCH> --background --launch agent --input -
```

Add `--parent <PARENT>` only when the intended parent is not already implied by
the source workspace. Supply one concise stdin prompt that tells the delegated
agent to load the active plan, implement only the named scope, keep the plan
synchronized, verify its work, and stop on material blockers.

Pass branch and parent names as separate, safely quoted argv values. Supply prompt
stdin with a literal quoted heredoc delimiter or an equivalent non-interpolating
mechanism; do not construct a shell program by concatenating user-owned values.

Treat `kmux add` as create-only and non-transactional after mutation begins. Never
retry automatically: an error can leave a valid branch, worktree, window, copied
files, hooks, or a running launcher.

## Stop after immediate handoff

Report only whether the single `kmux add` command returned successfully and the
workspace information it printed. A successful return means the configured
launcher process spawned; it does not mean delegated work completed or even
reached agent-specific readiness.

Do not poll `kmux status`, inspect the new pane/worktree, wait for completion,
summarize delegated progress, recursively delegate, commit, or push unless the
user separately requests that later action.

## Troubleshooting

- Unknown `agent` launcher: explain that the user must define the conventional
  launcher; do not substitute another profile silently.
- Missing child plan: stop and resolve tracked or configured copy/symlink
  transport before creating the workspace.
- No matching project session: report that delegation requires an existing tmux
  session for the Git project. Do not create a session or invent a headless
  runtime.
- Split project: report every session containing panes for the project. Ask the
  user to move, unlink, or close windows until the project appears in one session;
  do not choose a candidate.
- Mixed project session: report the conflicting project roots and ask the user to
  move or close windows until the session contains one Git project.
- Sandbox denial: report the denied sibling-worktree or tmux path and let the user
  adjust their runtime policy.
- Existing branch/worktree/workspace: report the exact conflict; do not remove or
  reuse it implicitly.
- Launcher spawn failure or timeout: explain that partial workspace state may
  remain and the existing shell window can be used for manual recovery.
