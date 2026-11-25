#!/usr/bin/env bash
# Checks everything a push is checked for: the format, the web app's build, the lints and the tests.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v mise >/dev/null || { echo "Install mise first: https://mise.jdx.dev"; exit 1; }
mise install
# The queries are checked against .sqlx/, never against a database.
export SQLX_OFFLINE=true

mise exec -- cargo fmt --check
mise exec -- npm --prefix apps/web ci
mise exec -- npm --prefix apps/web run build
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test --workspace
