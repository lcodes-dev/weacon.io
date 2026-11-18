#!/usr/bin/env bash
# Writes .sqlx/, which the build checks every query against. Run it after changing a query or a migration.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v mise >/dev/null || { echo "Install mise first: https://mise.jdx.dev"; exit 1; }
mise install
mkdir -p target
rm -f target/queries.db
export DATABASE_URL="sqlite://$PWD/target/queries.db"
mise exec -- cargo sqlx database setup --source apps/server/migrations
mise exec -- cargo sqlx prepare --workspace -- --all-targets
