# OpenCode integration

This directory contains the reference OpenCode server plugin for reporting session
state through `kmux set-agent-status` and an optional existing-server launcher. It
is integration code rather than part of the Rust core and targets the pinned
OpenCode `1.17.11` plugin and SDK contracts.

The server plugin is the only required status integration. OpenCode session-family
topology, status, title, context usage, directory, and deletion are reported by
`kmux-status-server.ts`. kmux resolves that directory to a canonical Git worktree,
matches the worktree to live tmux state, and displays one primary agent row per
worktree. No reporter-supplied pane identity is required.

`kmux-opencode-launcher.ts` is independent of the status plugin. It bootstraps an
optional prompt against an already-running server and then attaches the OpenCode
TUI. It never starts, owns, restarts, or stops an OpenCode server.

## Installation

The server entrypoint imports three adjacent runtime modules, so installations must
keep these files together:

```text
kmux-status-server.ts
kmux-server-reporter.ts
kmux-command-queue.ts
kmux-child-process.ts
```

The launcher source is also installed beside those files but does not import them.

When kmux is installed from the Nix package, integration files are installed under:

```text
$out/share/kmux/integrations/opencode/
```

The same package exposes a runnable wrapper at:

```text
$out/bin/kmux-opencode-launcher
```

That wrapper uses Bun from the Nix store with environment-file loading and runtime
package installation disabled. `opencode` intentionally remains an external
runtime and must be available on the launcher pane's `PATH`.

From a checkout, print the exact package output path with:

```sh
nix build --no-link --print-out-paths
```

Load only `kmux-status-server.ts` as the plugin entrypoint. OpenCode `1.17.11`
accepts an absolute TypeScript path in the `plugin` array, so an `opencode.json`
can reference either a checkout or the packaged Nix store path:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": [
    "/absolute/path/to/kmux-status-server.ts"
  ]
}
```

Keep the four status files above adjacent because the entrypoint imports the other three.
Using an explicit path also avoids placing helper modules in OpenCode's auto-loaded
plugin directory. The runtime requires `bun` and `kmux` on `PATH`. Restart OpenCode
after changing its plugin configuration, then use `kmux status` to confirm reports
are arriving.

## Existing-server launcher

Define a kmux launcher with an explicit server URL. Launcher names are
user-defined; a description lets users and agents identify its purpose through
`kmux config`:

```yaml
launchers:
  opencode:
    description: OpenCode attached to the existing local server
    command: kmux-opencode-launcher
    args:
      - --server-url
      - http://127.0.0.1:4096
```

The URL must be an HTTP or HTTPS origin only, without user information, a non-root
path, a query, or a fragment. Set `OPENCODE_SERVER_PASSWORD` to use Basic
authentication and optionally override the default `opencode` username with
`OPENCODE_SERVER_USERNAME`. Credentials remain in the inherited environment and
are never added to the attach command. Basic authentication is cleartext over
unencrypted HTTP; use HTTPS for non-loopback servers. A tmux server keeps the
environment from when it was started; restart it or deliberately update its
environment before launching if those variables were added later.

With a prompt, the adapter resolves the canonical worktree directory, creates one
directory-scoped session with `POST /session`, submits one asynchronous request to
`POST /session/:id/prompt_async`, and runs:

```text
opencode attach <URL> --dir <WORKTREE> --session <SESSION_ID>
```

Without a prompt, it does not create or choose a session and runs
`opencode attach <URL> --dir <WORKTREE>`. The TUI opens in that directory without
arbitrarily resuming an earlier session.

The adapter uses the server contracts present in OpenCode `1.17.11` and verified
with the `1.18.3` CLI. It expects session creation to return HTTP 200 with a
nonblank ID and the exact canonical directory, and asynchronous prompt acceptance
to return HTTP 204. It uses built-in `fetch`; it does not use `createOpencode`,
which would start another server. There is no local-server fallback.

API redirects are rejected so an HTTP 307/308 cannot replay a prompt to another
origin. While waiting for `opencode attach`, the adapter forwards catchable
`SIGINT`, `SIGHUP`, and `SIGTERM` signals and reaps the child before returning.
Terminal Ctrl-C, pane closure, and direct adapter termination therefore retain one
foreground process owner; `SIGKILL` remains uncatchable by definition.

`kmux workspace create` and `kmux workspace restore` report success after the
adapter process spawns, before server health, session creation, prompt execution,
or TUI attachment completes. Later errors remain visible in the workspace shell.
If session creation succeeds but prompt submission fails, the session is left
intact. If prompt submission succeeds but attach cannot start, headless work can
continue on the server. The adapter does not delete or abort either partial state
automatically.

OpenCode can request permissions or ask questions before attachment. Delayed
question recovery is supported by the tested server topology; permission behavior
still depends on the server's configured policy. Do not hide a request with fixed
attachment sleeps. Attach to the exact session to answer pending interaction.

The prompt is excluded from HTTP errors, logs, the `opencode attach` argv, and kmux
state, but it is the launcher adapter's final argv element for that process's
lifetime. Use `kmux workspace create ... --launcher-input -` to avoid placing it
in the original kmux caller argv; same-user process inspection of the adapter
remains an explicit exposure boundary.

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
lease policy. For one stale workspace row, select it in the kmux sidebar and press
`x`; kmux clears every current session represented by that row. Ongoing OpenCode
activity may recreate it immediately.

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
