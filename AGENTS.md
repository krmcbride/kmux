# kmux Project Instructions

These instructions apply to this repository in addition to the user-level agent rules.

## Tooling Notes

- In sandboxed agent environments, `bun` commands under `integrations/opencode` may fail with `CouldntReadCurrentDirectory` or `Cannot read directory "/home/": AccessDenied` because Bun walks parent dirs outside the sandbox. Treat this as expected and work around it by running local tool binaries directly when possible, such as `./node_modules/.bin/tsc` or `./node_modules/.bin/biome`.

## Rust Module Style

- Use `pub(crate)` primarily at crate boundary declarations in `src/lib.rs`, such as top-level modules that are internal to the crate. A `pub(crate) mod foo` bounds the effective visibility of `pub` items inside it, so `pub fn` or `pub struct` inside an internal module remains crate-reachable only.
- Inside regular modules, prefer normal Rust visibility: private by default, `pub` for the module's intended internal API, and narrower visibility such as `pub(super)` only for a clearly local boundary such as test support. Do not use `pub(crate)` as the default way to share items between internal modules.
- Outside `src/lib.rs`, keep `pub(crate)` only when Rust cannot express the intended boundary with ordinary module visibility, such as a crate-scoped `macro_rules!` re-export (`pub(crate) use my_macro;`), or when there is a documented crate-wide exception.
- When editing Rust visibility, search for `pub(crate)` outside `src/lib.rs`; each remaining instance should have a specific reason.
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

## Test Fixture Data

- Use neutral placeholder names in tests, examples, and fixtures, such as `project-alpha`, `example-repo`, `/repo/project`, `feature/sidebar`, and `ses_project_alpha`.
- Do not copy incidental local machine, client, person, repo, tmux session, or filesystem names from debugging output into committed tests or docs.

## Architecture Boundaries

- `src/workflows/` owns command use cases. Workflows orchestrate config, Git, tmux, state, files, and output, but should avoid becoming storage or adapter modules.
- `src/git.rs`, `src/tmux.rs`, `src/paths.rs`, and config/state modules are infrastructure adapters. Keep subprocess, filesystem, XDG, and Git common-dir details there instead of spreading them through workflow logic.
- Treat workflows as the application/use-case layer in a small hexagonal shape: CLI input and UI surfaces call into workflows, and workflows depend on adapters for Git, tmux, filesystem, config, and persistence.
- Keep UI code cordoned off as presentation. Terminal tables, sidebar/TUI rendering, and shell completion scripts should format or adapt data, not own workspace lifecycle rules, state mutation policy, or Git/tmux subprocess behavior.
- `src/state/agent/` is XDG-backed external agent observation persistence.
- `src/state/workspace.rs` is Git-common-dir-backed workspace graph persistence for local repo/worktree metadata.
