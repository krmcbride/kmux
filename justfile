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

opencode-plugin-fmt:
    cd integrations/opencode && ./node_modules/.bin/biome check --write .

opencode-plugin-check:
    cd integrations/opencode && ./node_modules/.bin/biome check .
    cd integrations/opencode && ./node_modules/.bin/tsc -p tsconfig.json --noEmit
    repo="$PWD"; scratch=/tmp/opencode/kmux-plugin-check; mkdir -p "$scratch"; cd "$scratch" && bun build "$repo/integrations/opencode/kmux-status-server.ts" --target bun --external "@opencode-ai/plugin" --external "@opencode-ai/sdk" --outfile "$scratch/kmux-status-server-check.js"
    repo="$PWD"; scratch=/tmp/opencode/kmux-plugin-check; mkdir -p "$scratch"; cd "$scratch" && bun build "$repo/integrations/opencode/kmux-status-tui.ts" --target bun --external "@opencode-ai/plugin/tui" --outfile "$scratch/kmux-status-tui-check.js"

check: fmt-check clippy test opencode-plugin-check
