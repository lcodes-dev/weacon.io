#!/usr/bin/env bash
# Sets up and runs the server on :8080 and the web app on :5173. Ctrl-C stops both.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v mise >/dev/null || { echo "Install mise first: https://mise.jdx.dev"; exit 1; }
mise install
mise exec -- npm --prefix apps/web install
mkdir -p .dev apps/web/dist

trap 'kill 0' EXIT
WEACON_DB="$PWD/.dev/weacon.db" mise exec -- cargo run -p weacon-server &
mise exec -- npm --prefix apps/web run dev &
wait
