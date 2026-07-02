# kmux Project Instructions

These instructions apply to this repository in addition to the user-level agent rules.

## Rust Module Style

- Use `pub(crate)` primarily at crate boundary declarations in `src/lib.rs`, such as top-level modules that are internal to the crate.
- Inside regular modules, prefer normal Rust visibility: private by default, `pub` for the module's intended internal API, and narrower visibility such as `pub(super)` only for a clearly local boundary such as test support.
- Keep public items before private items where practical:
  - public structs, enums, and type aliases before private helper types
  - public impl methods before private impl methods
  - public free functions before private helper functions
- Keep tests at the bottom of each module behind `#[cfg(test)] mod tests`.
- Extract shared test setup into `test_support` modules when it improves readability or avoids broadening production visibility.
- Do not widen production visibility only for tests. Same-module tests should exercise private helpers directly; shared test-only constructors, fixtures, and helpers should live behind `#[cfg(test)]` in a local `test_support` module or a test-only impl on the owning type.
- Avoid broad utility modules. Put behavior in the module that owns the concept.
- Use inner module doc comments (`//!`) at the top of `mod.rs` files or focused module files when a module needs ownership, boundary, or upstream-integration context.
- Add Rust doc comments to public functions and methods to explain the behavior, invariants, and side effects that are not obvious from the signature. Add brief comments to non-trivial private helpers when they encode workflow policy, parsing rules, filesystem layout, subprocess behavior, or other mental-model context useful during code review.

## Architecture Boundaries

- `src/workflows/` owns command use cases. Workflows orchestrate config, Git, tmux, state, files, and output, but should avoid becoming storage or adapter modules.
- `src/git.rs`, `src/tmux.rs`, `src/paths.rs`, and config/state modules are infrastructure adapters. Keep subprocess, filesystem, XDG, and Git common-dir details there instead of spreading them through workflow logic.
- Treat workflows as the application/use-case layer in a small hexagonal shape: CLI input and UI surfaces call into workflows, and workflows depend on adapters for Git, tmux, filesystem, config, and persistence.
- Keep UI code cordoned off as presentation. Terminal tables, sidebar/TUI rendering, and shell completion scripts should format or adapt data, not own workspace lifecycle rules, state mutation policy, or Git/tmux subprocess behavior.
- `src/state/agent/` is XDG-backed external agent observation persistence.
- `src/state/workspace.rs` is Git-common-dir-backed workspace graph persistence for local repo/worktree metadata.
