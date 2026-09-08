# kmux

kmux is a tmux and Git worktree workflow helper. It opens registered worktrees
in a project tmux session, manages the worktrees it creates, and shows agent
activity across worktrees in a global sidebar.

kmux is currently pre-release. Its CLI and persisted state may still change
before the first stable release.

## Requirements and installation

kmux expects `git` and `tmux` on `PATH`. The optional OpenCode launcher also
expects `opencode` on the launcher pane's `PATH`; its Nix wrapper carries its own
Bun runtime.

Install from a checkout with Nix:

```sh
nix profile install .
```

The Nix package includes shell completions, the OpenCode integration and launcher,
and the reference delegation skill. To install only the Rust binary with Cargo
instead:

```sh
cargo install --path .
```

Run workspace lifecycle commands from a Git repository with an existing tmux
server. Running inside that project's tmux session remains the normal interactive
path. Detached callers resolve the same session from its live Git pane paths.

## Workspace model

A workspace is a registered Git worktree with a stable kmux identity. Git supplies
its path, HEAD, branch or detached state, and registration flags. Kmux separately
records its label, lifecycle authority, retention, presentation intent, and lineage.
External worktrees can live anywhere and remain external when opened in tmux.

Existing branch-derived kmux worktrees migrate once to owned persistent records
without moving directories or changing branches. New registrations never gain
authority from a path or branch name. Ownership is bound to the original Git
registration, so a replacement at the same path cannot inherit it.

The release model distinguishes one Git project from its worktree workspaces:

```text
Git project (one canonical Git common repository)
├── project tmux session (0..1)
│   ├── kmux-managed workspace windows (0..1 per workspace)
│   └── temporary windows and panes
└── workspaces (one per canonical Git worktree root)
    └── agent sessions (0..*)
        └── reporters (1..*)
```

Agent activity collapses to at most one displayed row per workspace.

Kmux treats each tmux session as one Git project bucket. Non-Git scratch panes
are neutral, but a project split across sessions or a session containing multiple
Git projects is an error that must be repaired before workspace mutation.

Use separate worktrees for parallel agent work. Concurrent agent sessions in one
worktree are supported, but kmux chooses one primary agent session for display
using status, recency, and deterministic tie-breakers.

## Workspace lifecycle

Create an ephemeral workspace from any checkout, including a detached one:

```sh
tmux new-session -s work
cd /repo/project-alpha
kmux workspace create
kmux workspace create --from main --name review-alpha
```

Without a branch argument, creation starts detached at the invoking checkout's
HEAD (or `--from <REF>`) and creates no branch. Storage defaults to
`~/.kmux/worktrees/<opaque-id>/<repo-basename>`. The random directory is reserved
exclusively across projects; an optional `--name` sets only the display label.
Set an absolute or `~/`-prefixed `worktree_root` in configuration to change where
future ephemeral worktrees are created. Existing paths remain unchanged.

The table and tmux window name identify ephemeral retention explicitly. Creating,
switching, renaming, or publishing branches inside the worktree keeps its identity,
path, and retention. Multiple publication branches can belong to one environment.

Supplying a branch selects the persistent compatibility preset:

```sh
kmux workspace create feature/sidebar
```

This creates a new local branch and a sibling worktree at
`<repo>__worktrees/feature-sidebar`. By default the current branch is the parent;
use `--parent <BRANCH>` to override it. A known `REMOTE/BRANCH` creates a local
tracking branch. New windows receive focus; callers outside the target tmux
session must pass `--background` and otherwise fail before creation.

Keep an ephemeral workspace persistently in its current directory:

```sh
kmux workspace promote
kmux workspace promote review-alpha --name long-running
```

Promotion works while detached or on a branch. It preserves the workspace ID,
path, HEAD, branches, presentation intent, and running panes. The optional name
changes its label and window name. It creates no branch and grants no branch
deletion authority. Primary, external, and already persistent workspaces cannot
be promoted. There is no automatic cleanup or relocation.

When a default launcher is configured, kmux starts it as the foreground program
in the new window after file operations, `post_create`, and parent metadata are
complete. The pane's configured shell remains its long-lived process and resumes
when the launcher exits. Override it for one create without changing the default:

```sh
kmux workspace create feature/review --launcher editor
kmux workspace create feature/delegated --background --launcher review-agent \
  --launcher-input "Review the current test harness and report correctness risks"
```

`--launcher-input` requires an explicit `--launcher`. It appends one literal final
argument after the launcher's configured arguments. Omitted input adds no argument;
an explicit empty value adds one empty argument. Exactly `--launcher-input -` reads
all caller stdin as UTF-8 without trimming, which is preferable for multiline text
or when the input should not appear in the original kmux process argv.

Kmux considers the create successful once the launcher process spawns. It does not
wait for launcher-specific readiness or completion. The shell gets three seconds
to consume the ingress request, followed by a fresh three seconds to acknowledge
launcher spawn. A spawn failure or timeout returns an error but deliberately
leaves the created branch, worktree, setup effects, parent metadata, and usable
shell window in place for manual recovery. A later launcher failure is visible in
that shell and does not retroactively fail `kmux workspace create`.

The one-shot request lives briefly under the owner-only
`${XDG_STATE_HOME:-$HOME/.local/state}/kmux/launcher-runtime` directory on Linux
and macOS so a sandboxed caller and an existing tmux server can access the same
capability even when they have different `/tmp` or `XDG_RUNTIME_DIR` mounts.
Kmux consumes the request before launcher spawn and removes its private directory
after handoff or failure.

Inspect workspace state as a table or JSON:

```sh
kmux workspace list
kmux workspace list --json
```

Change the recorded parent of the current workspace, or name an explicit child:

```sh
kmux workspace set-parent main
kmux workspace set-parent main feature/sidebar
```

If worktrees still exist after restarting tmux or closing windows, restore their
expected windows:

```sh
kmux workspace restore
```

Restore opens every live external worktree registered to the project, even if it
has never been opened in kmux. It also restores remembered owned presentations.
New windows use the current default launcher without prior one-shot input;
existing windows keep running, including after an earlier launcher failure.
Without a default launcher, windows contain the configured tmux shell.

Open one checkout by path, workspace ID, label, or current branch:

```sh
kmux workspace open /repo/external-worktree
kmux workspace open --background --launcher editor
kmux workspace close /repo/external-worktree
```

With no target, open and close use the current registered checkout, including
detached worktrees. Open remembers presentation and accepts the same launcher
input as create. Existing windows are focused or reused without restarting their
launcher. Close forgets that presentation and closes only its managed window,
leaving files, commits, branches, and the worktree registration intact. A live
external worktree remains eligible for the next restore after close. Stale or
prunable registrations are listed as unavailable and never pruned by restore.

Remove a workspace by branch, slug, or window name. From inside a kmux worktree,
the name may be omitted:

```sh
kmux workspace remove feature/sidebar
kmux workspace remove
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
the sidebar recommends `kmux workspace restore` for managed workspaces. If matching
windows span multiple tmux sessions, kmux reports the ambiguity instead of choosing
an arbitrary destination.

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
launcher/server setup, behavior, diagnostics, and pre-release state reset guidance.

The Nix package also installs the generic `delegating-with-kmux` skill under
`$out/share/kmux/skills/delegating-with-kmux/`. Package share directories are not
agent discovery roots. Copy or symlink that directory into a supported location,
such as `~/.agents/skills/delegating-with-kmux/`, then restart or refresh the agent
runtime. The skill acts only after an explicit delegation request, discovers
available launchers from the active kmux configuration, and asks the user when no
launcher clearly matches the requested work.

After `nix profile install .`, the packaged source is normally available at
`~/.nix-profile/share/kmux/skills/delegating-with-kmux/`. Install it without
overwriting an existing skill:

```sh
test ! -e ~/.agents/skills/delegating-with-kmux
mkdir -p ~/.agents/skills
ln -s ~/.nix-profile/share/kmux/skills/delegating-with-kmux \
  ~/.agents/skills/delegating-with-kmux
```

For a build without profile installation, use the exact output path:

```sh
out=$(nix build --no-link --print-out-paths)
test ! -e ~/.agents/skills/delegating-with-kmux
mkdir -p ~/.agents/skills
ln -s "$out/share/kmux/skills/delegating-with-kmux" \
  ~/.agents/skills/delegating-with-kmux
```

## Configuration

On Linux and macOS, kmux uses the same Unix XDG directory policy:

| Purpose | Absolute override | Default |
| --- | --- | --- |
| Configuration | `XDG_CONFIG_HOME` | `$HOME/.config` |
| Persistent state | `XDG_STATE_HOME` | `$HOME/.local/state` |
| Cache | `XDG_CACHE_HOME` | `$HOME/.cache` |

Empty or relative XDG overrides are ignored because XDG base directories must be
absolute. Windows continues to use its platform-native directories when an
absolute override is absent.

The resulting kmux paths are
`$HOME/.config/kmux/config.yaml`, `$HOME/.local/state/kmux`, and
`$HOME/.cache/kmux` by default. The state root includes persistent agent
observations and launcher handoffs; opt-in telemetry uses the cache root. Because
kmux is pre-release, macOS users upgrading from the previous behavior should note
that kmux no longer reads `~/Library/Application Support/kmux` and does not
automatically migrate or merge data from that location.

kmux loads strict YAML configuration from the resolved config path. A missing
file uses defaults. Inspect the fully-resolved active values as YAML or JSON:

```sh
kmux config
kmux config --json
```

For example, the source configuration can contain:

```yaml
window_prefix: kmux-

window:
  default_launcher: editor

launchers:
  editor:
    description: Open the workspace in Neovim
    command: nvim

  review-agent:
    description: Review project changes and report findings
    command: example-agent-launcher
    args:
      - --existing-server
      - http://127.0.0.1:4096

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

Launcher names must match `^[a-z0-9]+(?:[-_][a-z0-9]+)*$`. Each launcher has an
optional nonblank `description`, one nonblank executable in `command`, and an
optional ordered `args` list. Descriptions help users and agents select a launcher
for a task. Commands and arguments are exact argv values, not shell fragments:
quotes, whitespace, wildcards, redirections, and metacharacters remain literal.
Put complex shell behavior in an explicit script or configure a shell executable
and arguments yourself.

Launchers run with the new worktree as cwd and inherit the pane shell's
environment and TTY streams. Bare commands resolve through that environment's
`PATH`; absolute paths remain absolute; relative paths containing separators are
resolved from the worktree. The optional final input also appears in the launcher
adapter's argv for that process's lifetime, so same-user process inspection is an
explicit exposure boundary even when `--launcher-input -` protects the original
kmux argv.

`post_create` remains a separate ordered list of shell commands. Configuration
also supports repo-relative files to copy or symlink before launcher startup.
Post-create hooks run while kmux owns the project's lifecycle transaction.
Recursive `kmux workspace create`, `kmux workspace restore`,
`kmux workspace remove`, or `kmux workspace set-parent` calls fail immediately
instead of waiting on their parent process. The same guard applies to synchronous
Git hooks and checkout filters invoked by kmux. Run nested lifecycle operations
after the original command returns.
Unknown fields, invalid paths, invalid launcher references, NUL values, and blank
launcher descriptions or commands are rejected rather than ignored.

## Shell completions

Generate completion scripts for Bash, Elvish, Fish, PowerShell, or Zsh:

```sh
kmux completions <SHELL>
```

Bash, Fish, and Zsh completions include dynamic branch, workspace, and configured
launcher candidates. The Nix package installs those three completion scripts
automatically.

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
