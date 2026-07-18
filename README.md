# kmux

kmux is a tmux and Git worktree workflow helper. It creates one focused
workspace per branch, restores the tmux windows those workspaces expect, and
shows agent activity across worktrees in a global sidebar.

kmux is currently pre-release. Its CLI and persisted state may still change
before the first stable release.

## Requirements and installation

kmux expects `git` and `tmux` on `PATH`. The bundled OpenCode integration also
requires `bun`.

Install from a checkout with Nix:

```sh
nix profile install .
```

The Nix package includes shell completions and the OpenCode integration files.
To install only the binary with Cargo instead:

```sh
cargo install --path .
```

Run workspace lifecycle commands from a Git repository inside tmux.

## Workspace model

A kmux workspace is identified by its canonical Git worktree root. For a main
checkout at `/repo/project-alpha`, kmux places linked worktrees under the sibling
directory `/repo/project-alpha__worktrees/` and derives filesystem/tmux slugs
from branch names.

The release model is intentionally worktree-centered:

- One canonical Git root identifies one workspace.
- A workspace expects zero or one kmux-managed tmux window.
- Temporary windows in the same tmux session may also contain panes rooted in
  that workspace.
- A workspace may have zero or more agent sessions, each with multiple
  reporters.
- Agent activity collapses to at most one displayed row per workspace.

Use separate worktrees for parallel agent work. Concurrent sessions in one
worktree are supported, but kmux chooses one primary session for display using
status, recency, and deterministic tie-breakers.

## Workspace lifecycle

Start in the main checkout and create a branch workspace:

```sh
tmux new-session -s work
cd /repo/project-alpha
kmux add feature/sidebar
```

`kmux add` creates a new local branch, a linked worktree, and a tmux window. By
default the current branch is recorded as the parent and the new window receives
focus. Use `--parent <BRANCH>` or `--background` to override those choices.

Inspect workspace state as a table or JSON:

```sh
kmux list
kmux list --json
```

Change the recorded parent of the current workspace, or name an explicit child:

```sh
kmux parent main
kmux parent main feature/sidebar
```

If worktrees still exist after restarting tmux or closing windows, restore their
expected windows:

```sh
kmux restore
```

Remove a workspace by branch, slug, or window name. From inside a kmux worktree,
the name may be omitted:

```sh
kmux remove feature/sidebar
kmux remove
```

Removal deletes the linked worktree, local branch, expected tmux window, and the
workspace's own parent link. It refuses dirty worktrees or branches that are not
safely merged unless `--force` is supplied.

## Sidebar and agent activity

Enable, disable, or explicitly toggle the global sidebar:

```sh
kmux sidebar on
kmux sidebar off
kmux sidebar toggle
```

The sidebar is global to the tmux server, not scoped to the current repository.
When enabled, kmux reconciles one sidebar pane in every physical window,
including scratch windows that are not managed workspaces. Running
`kmux sidebar on` again is safe and reapplies sidebar configuration.

Sidebar keys:

- `j`/`Down` and `k`/`Up`: move between workspace rows.
- `g` and `G`: select the first or last row.
- `Enter`: jump to the selected workspace.
- `x`: clear all agent sessions currently represented by the selected row.
- `F5`: request an activity refresh.
- `q`, `Esc`, or `Ctrl-C`: disable the global sidebar.

For a jump, kmux only chooses among windows whose pane directories resolve to
the selected Git root. It prefers a current matching window, then tmux's previous
matching window, then a deterministic matching fallback. Within that window it
prefers an active or previous matching non-sidebar pane before a deterministic
pane fallback. If only the sidebar remains, the window jump still succeeds.

A row remains visible when agent activity exists but no matching window is live;
the sidebar recommends `kmux restore` for managed workspaces. If matching windows
span multiple tmux sessions, kmux reports the ambiguity instead of choosing an
arbitrary destination.

## Status and integrations

`kmux status` is a global activity export across repositories:

```sh
kmux status
kmux status --git
kmux status --json
```

Integrations report observations through the supported
`kmux set-agent-status` command. A minimal generic report looks like:

```sh
kmux set-agent-status working \
  --agent-kind example-agent \
  --session-id session-123 \
  --reporter-kind server \
  --reporter-instance local \
  --title "Reviewing changes" \
  --directory "$PWD"
```

The agent/session pair identifies one logical session. Reporter kind and instance
identify one independently replaceable observation for that session. Directory
is the primary attachment hint; kmux resolves it to a canonical Git root rather
than trusting reporter-supplied tmux identities.

The bundled reference integration reports OpenCode session activity. See
[`integrations/opencode/README.md`](integrations/opencode/README.md) for
installation, behavior, diagnostics, and pre-release state reset guidance.

## Configuration

kmux loads strict YAML configuration from
`${XDG_CONFIG_HOME:-$HOME/.config}/kmux/config.yaml`. A missing file uses
defaults. For example:

```yaml
window_prefix: kmux-

status_icons:
  working: "🤖"
  waiting: "💬"
  done: "✅"
  sleeping: "💤"

sidebar:
  idle_after_seconds: 1800
  width:
    min: 36
    percent: 20
    max: 52
```

Configuration also supports a startup pane command, post-create commands, and
repo-relative files to copy or symlink into new worktrees. Unknown fields and
invalid paths are rejected rather than ignored.

## Shell completions

Generate completion scripts for Bash, Elvish, Fish, PowerShell, or Zsh:

```sh
kmux completions <SHELL>
```

Bash, Fish, and Zsh completions include dynamic branch and workspace candidates.
The Nix package installs those three completion scripts automatically.

## Recovery and stale activity

kmux does not currently expire agent observations with a TTL or lease. A clean
integration shutdown should remove owned observations, but a process crash may
leave a valid stale row. Select it in the sidebar and press `x` to remove every
session currently represented by that workspace row. Ongoing reporter activity
may recreate the row immediately; this is expected.

The OpenCode integration documents the broader pre-release XDG state reset for
identity or schema changes. That reset affects every agent integration, so
inspect or back up the directory before removing anything.

## Development

Enter the project toolchain and run the complete check suite:

```sh
nix develop
just check
```
