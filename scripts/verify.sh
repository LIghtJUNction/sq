#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
uv sync --project python --frozen
SQ_TEST_BINARY="$PWD/target/debug/sq" uv run --directory python pytest -q
uv run --directory python ruff check src tests
uv run --directory python ruff format --check src tests
uv run --directory python mypy src
