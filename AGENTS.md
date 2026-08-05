# kmux Project Instructions

These instructions apply to this repository in addition to the user-level agent rules.

## Tooling Notes

- In sandboxed agent environments, `bun` commands under `integrations/opencode` may fail with `CouldntReadCurrentDirectory` or `Cannot read directory "/home/": AccessDenied` because Bun walks parent dirs outside the sandbox. Treat this as expected. Local `./node_modules/.bin/tsc` and `./node_modules/.bin/biome` binaries run from that directory can cover the corresponding subchecks, but they do not replace Bun-based installation, tests, or bundle checks; report those checks as unverified when the sandbox blocks them.

## Rust Module Style

- Use `pub(crate)` primarily at crate boundary declarations in `src/lib.rs`, such as top-level modules that are internal to the crate. A `pub(crate) mod foo` bounds the effective visibility of `pub` items inside it, so `pub fn` or `pub struct` inside an internal module remains crate-reachable only.
- Inside regular modules, prefer normal Rust visibility: private by default, `pub` for the module's intended internal API, and narrower visibility such as `pub(super)` only for a clearly local boundary such as test support. Do not use `pub(crate)` as the default way to share items between internal modules.
- Outside `src/lib.rs`, keep `pub(crate)` only when Rust cannot express the intended boundary with ordinary module visibility, such as a crate-scoped `macro_rules!` re-export (`pub(crate) use my_macro;`), or when there is a documented crate-wide exception.
- When editing Rust visibility, search for `pub(crate)` outside `src/lib.rs`; each remaining instance should have a specific reason.
- Keep public items before private items where practical:
  - public structs, enums, and type aliases before private helper types
  - public impl methods before private impl methods
  - public free functions before private helper functions
- Keep unit tests at the bottom of each module behind `#[cfg(test)] mod tests`.
- Extract shared unit-test setup into `test_support` modules when it improves readability or avoids broadening production visibility.
- Do not widen the default supported API only for tests. Same-module unit tests should exercise private helpers directly; shared test-only constructors, fixtures, and helpers should live behind `#[cfg(test)]` in a local `test_support` module or a test-only impl on the owning type.
- Avoid broad utility modules. Put behavior in the module that owns the concept.
- Use inner module doc comments (`//!`) at the top of `mod.rs` files or focused module files when a module needs ownership, boundary, or upstream-integration context.
- Add Rust doc comments to public functions and methods to explain the behavior, invariants, and side effects that are not obvious from the signature. Add brief comments to non-trivial private helpers when they encode workflow policy, parsing rules, filesystem layout, subprocess behavior, or other mental-model context useful during code review.

## Test Fixture Data

- Use neutral placeholder names in tests, examples, and fixtures, such as `project-alpha`, `example-repo`, `/repo/project`, `feature/sidebar`, and `ses_project_alpha`.
- Do not copy incidental local machine, client, person, repo, tmux session, or filesystem names from debugging output into committed tests or docs.

## Testing Boundaries

- Keep `cargo test --lib` process-free. Unit tests should exercise policy, parsing, and orchestration with explicit facts, focused fakes, and deterministic in-process or filesystem fixtures; do not launch Git, tmux, shells, launchers, or other subprocesses.
- Exercise real Git, tmux, launcher, and sidebar process behavior through the explicit adapter contract targets gated by `internal-adapter-contract-tests`. Keep the `#[doc(hidden)]` harness surface narrow, and do not change runtime behavior when the feature is enabled.
- Keep process-backed tests hermetic: clear inherited environment state, use owned HOME/XDG/TMP and Git configuration, use private tmux sockets, wait on observable readiness with bounded timeouts, and clean up owned processes. Never contact the developer's default tmux server or Git configuration, and fail clearly rather than skipping when a required external tool is unavailable.
- Keep black-box integration support under `tests/support` and feature-gated contract support beside the adapter that owns it. Share fixtures within those boundaries, but do not broaden APIs or couple the layers solely to deduplicate similar setup.
- Use the flake toolchain and `just check` for full verification. `just test-lib` runs the process-free library suite, `just adapter-contracts` runs process-backed adapter contracts, and `just test` runs the complete Rust suite; `cargo test` alone excludes the feature-gated adapter contracts.

## Architecture Boundaries

- `src/workflows/` owns command use cases. Workflows orchestrate config, Git, tmux, state, files, and output, but should avoid becoming general storage or adapter modules. `src/workflows/files.rs` is the focused owner for configured worktree file operations and post-create command execution.
- `src/git.rs`, `src/tmux.rs`, `src/launcher.rs`, `src/paths.rs`, and config/state modules are infrastructure boundaries. Keep reusable subprocess protocols, filesystem layout, XDG, and Git common-dir details there instead of spreading them through workflow logic.
- Treat workflows as the application/use-case layer in a small hexagonal shape: CLI input and UI surfaces call into workflows, and workflows depend on adapters for Git, tmux, filesystem, config, and persistence.
- Keep rendering and UI-state modules presentation-only. Sidebar action and lifecycle modules may coordinate tmux and persisted-state effects, and workflows may own command-local stdout formatting; renderers and completion scripts should not own workspace lifecycle or state-mutation policy.
- `src/agent/` owns external agent observation, query, status, session aggregation, and sidebar behavior.
- `src/state/agent/` is XDG-backed external agent observation persistence.
- `src/state/workspace.rs` is Git-common-dir-backed workspace graph persistence for local repo/worktree metadata.
