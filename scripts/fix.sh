#!/usr/bin/env bash
# Formats the code and applies the lint fixes clippy is sure of.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked --quiet --fix --allow-dirty --allow-staged
cargo fmt --all
