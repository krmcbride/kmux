# Editor settings

This directory is intentional even when VS Code is not in use. Rustaceanvim
loads `.vscode/settings.json` and passes its Cargo feature selection to
rust-analyzer, keeping the feature-gated adapter contract code active for IDE
analysis.

The setting does not change normal Cargo build behavior. A native
`rust-analyzer.toml` would be more editor-neutral, but rust-analyzer does not yet
reliably propagate `cargo.features` from that file into its project model.
