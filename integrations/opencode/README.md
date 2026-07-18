# OpenCode integration

This directory contains the reference OpenCode server plugin for reporting session
state through `kmux set-agent-status`. It is integration code rather than part of
the Rust core and targets the pinned OpenCode `1.17.11` plugin and SDK contracts.

The server plugin is the only required status integration. OpenCode session-family
topology, status, title, context usage, directory, and deletion are reported by
`kmux-status-server.ts`. kmux resolves that directory to a canonical Git worktree,
matches the worktree to live tmux state, and displays one primary agent row per
worktree. No TUI plugin or reporter-supplied pane identity is required.

## Installation

The server entrypoint imports three adjacent runtime modules, so installations must
keep these files together:

```text
kmux-status-server.ts
kmux-server-reporter.ts
kmux-command-queue.ts
kmux-child-process.ts
```

When kmux is installed from the Nix package, integration files are installed under:

```text
$out/share/kmux/integrations/opencode/
```

Reference `kmux-status-server.ts` from OpenCode configuration, either directly from
a checkout during development or from the packaged Nix store path in declarative
configuration. The runtime requires `bun` and `kmux` on `PATH`.

## Behavior and diagnostics

OpenCode creates one plugin instance per directory. Each reporter uses the public
directory-scoped `event` and `dispose` hooks and identifies its observations by the
OpenCode server URL plus directory, preventing one directory's disposal from
clearing another reporter's state.

Initial session and status snapshots are bounded. The pinned OpenCode server route
accepts a session-list limit that its generated v1 SDK type omits; the adapter keeps
that compatibility seam local and deliberately loads the 200 most recently updated
sessions. Events received during bootstrap are replayed afterward, so newer live
state wins. kmux commands remain ordered, have bounded child-process lifetimes, and
are retried by a later equivalent event after transient failure. Disposal stops
event intake, cleans up owned observations, and returns after a bounded drain even
if a child process does not exit.

Reporting failures never make OpenCode unusable. Diagnostics are written through
OpenCode structured logging under the `kmux-status-server` service. Repeated
identical command failures are logged only on transition, with a recovery entry
after successful delivery. Logs include safe operation, session, exit-code, and
bounded error metadata rather than full commands, titles, directories, or event
payloads.

## Pre-release state reset

Removing the former TUI reporter and changing the server reporter identity can
leave old observations that the new reporter does not own. Before using this
pre-release integration, remove the contents of the kmux agent-observation state
directory while OpenCode is stopped:

```text
${XDG_STATE_HOME:-$HOME/.local/state}/kmux/agent-observations/
```

On platforms without an XDG state directory, kmux uses the platform local-data
directory under `kmux/agent-observations/`. This reset removes observations from all
agent integrations, not only OpenCode, so inspect or back up the directory when
other reporters matter. No automatic migration is provided for pre-release state.

A clean plugin disposal removes observations it successfully owns. A process crash
can still leave valid observations because kmux does not currently apply a TTL or
lease policy.

## Development

The plugin sources are an isolated Bun/TypeScript package:

```sh
bun install
bun run check
```

From the kmux repository root, the same check is available as:

```sh
just opencode-plugin-check
```
