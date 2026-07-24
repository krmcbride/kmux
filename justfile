set positional-arguments
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-targets --features internal-adapter-contract-tests -- -D warnings

test-lib:
    cargo test --lib

adapter-contracts:
    cargo test --features internal-adapter-contract-tests \
        --test git_adapter_contracts \
        --test tmux_adapter_contracts \
        --test launcher_adapter_contracts \
        --test sidebar_action_contracts

test:
    cargo test
    just adapter-contracts

build:
    cargo build

opencode-plugin-install:
    cd integrations/opencode && bun install --frozen-lockfile

opencode-plugin-fmt: opencode-plugin-install
    cd integrations/opencode && bun run fmt

opencode-plugin-check: opencode-plugin-install
    cd integrations/opencode && bun run check

check: fmt-check clippy test opencode-plugin-check
