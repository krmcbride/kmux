# kmux

Personalized and simplified workmux, geared toward tmux and git worktrees while being as lean as possible.

This repository is currently being scaffolded. The first implementation target is a Rust CLI that manages git worktrees, tmux windows, status state, and sidebar presentation without the multi-backend, sandbox, agent-installer, dashboard, or PR-oriented parts of upstream workmux.

Agent and editor integrations should be examples layered on top of stable extension points, such as `kmux set-window-status`, rather than assumptions baked into the core CLI.

## Development

```bash
nix develop
just check
```

Build the package with:

```bash
nix build
```
