# OpenCode integration

This directory contains reference OpenCode plugins for reporting session state to
kmux through the generic `kmux set-agent-status` command. They are maintained as
integration code, not as part of the Rust core.

When kmux is installed from the Nix package, these files are installed under:

```text
$out/share/kmux/integrations/opencode/
```

The plugins are intended to be referenced from OpenCode configuration, either
directly from a checkout during development or from the packaged Nix store path
in declarative Home Manager configuration.

The status plugins report the active OpenCode session directory as the primary
kmux location. When OpenCode exposes a workspace ID, the plugins also report it
as optional routing metadata so the selection hook can scope OpenCode TUI
navigation safely.

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
