# Adapter contract tests

These targets exercise kmux's real Git, tmux, sidebar-action, and launcher process adapters. They
are intentionally separate from `cargo test --lib`, which must remain a
process-free policy and parsing suite.

Run all contracts with:

```console
just adapter-contracts
```

The non-default `internal-adapter-contract-tests` feature exposes only the
unsupported harness entry points required by these targets. It does not select a
different runtime implementation.

The contracts require Git and tmux on `PATH`. Unix launcher contracts also
require a POSIX `/bin/sh`, executable temporary files, symlinks, and Unix
permission semantics. Tmux contracts use a private socket directory, an empty
configuration, a fixed shell, readiness polling, and automatic server cleanup.
Git contracts clear the child environment and use private HOME, XDG, temporary,
configuration, identity, and hook paths.

Linux and Darwin are declared Nix targets. Platform-specific contracts use
`#[cfg(unix)]`; non-Unix builds retain the portable protocol, parsing, and policy
coverage.
