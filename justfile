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

test:
    cargo test

build:
    cargo build

opencode-plugin-install:
    cd integrations/opencode && bun install --frozen-lockfile

opencode-plugin-fmt: opencode-plugin-install
    cd integrations/opencode && bun run fmt

opencode-plugin-check: opencode-plugin-install
    cd integrations/opencode && bun run check

check: fmt-check clippy test opencode-plugin-check
