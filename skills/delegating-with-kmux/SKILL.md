---
name: delegating-with-kmux
description: >-
  Use when the user explicitly asks to delegate project work with kmux, launch
  another agent in a new kmux worktree, or set up a kmux workspace for an agent.
---

## Delegate

Act only after an explicit request to delegate with kmux or create a kmux
workspace for another agent. Do not infer delegation from task size.

1. Discover configured launcher names and descriptions:

```sh
kmux config --json |
  jq '.launchers | to_entries | map({name: .key, description: .value.description})'
```

If `jq` is unavailable, inspect the JSON directly. Choose the launcher whose name
or description matches the task; ask the user when there is no clear match.

2. Write a concise, self-contained prompt. Default to an ephemeral detached
   workspace; supply a branch only when a persistent branch workspace is requested. A new
   worktree starts from committed Git history, so do not assume uncommitted or
   untracked files will be present.

3. Set `LAUNCHER` to the chosen value, then run exactly one creation command.
   Add `--from <REF>` or a display `--name <LABEL>` when needed. The optional
   persistent branch preset accepts `--parent <BRANCH>` instead of `--from`.

```sh
kmux workspace create --background \
  --launcher "$LAUNCHER" --launcher-input - <<'KMUX_PROMPT'
Task: <objective and expected result>
Context: <relevant issue, files, commits, and project facts>
Scope: <boundaries, constraints, and whether edits are allowed>
Verification: <checks to run or evidence to return>
KMUX_PROMPT
```

4. Report the command result and workspace information it prints. Do not retry,
   repair conflicts, poll, inspect the child, commit, or push unless the user asks.

Do not modify or commit source-workspace data merely to transport it. Never put
credentials or other secrets in launcher input.

If command behavior is unclear, consult `kmux workspace create --help` or broader
`kmux --help`; the CLI help is authoritative.
