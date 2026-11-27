#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

command -v mise >/dev/null || { echo "Install mise first: https://mise.jdx.dev"; exit 1; }
mise install
export SQLX_OFFLINE=true

mise exec -- cargo fmt --check
mise exec -- npm --prefix apps/web ci
mise exec -- npm --prefix apps/web run build
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test --workspace
mise exec -- npm --prefix apps/web exec -- playwright install --with-deps chromium
mise exec -- npm --prefix apps/web run phone-check
