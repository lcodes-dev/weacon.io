#!/usr/bin/env bash
# Runs every check CI runs, in the same order. Each must pass.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/tools.sh
. scripts/tools.sh
use_mise

step() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  cargo "$1" --version >/dev/null 2>&1 || die "cargo $1 is missing; run scripts/setup.sh"
}

rust_checks() {
  step "Formatting"
  cargo fmt --all --check

  step "Lints"
  cargo clippy --workspace --all-targets --all-features --locked --quiet -- -D warnings

  step "Docs"
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked --quiet

  step "Unused dependencies"
  need machete
  cargo machete

  step "Licences, advisories and sources"
  need deny
  cargo deny --locked check --hide-inclusion-graph

  step "Tests"
  cargo test --workspace --all-features --locked
}

other_checks() {
  step "Extension and TypeScript tooling: types, generated types and tests"
  local node_version
  node_version="v$(cat .node-version)"
  [ "$(node --version 2>/dev/null || true)" = "$node_version" ] || die "Weacon's TypeScript needs Node ${node_version#v}"
  [ -x apps/extension/node_modules/.bin/tsc ] || die "the extension's packages are missing; run scripts/setup.sh"
  npm --prefix apps/extension run --silent check
  (cd tooling/ts && node --test)

  step "Web app: types, generated types, build, tests and end-to-end tests in Chromium"
  [ -x apps/web/node_modules/.bin/tsc ] || die "the web app's packages are missing; run scripts/setup.sh"
  npm --prefix apps/web run --silent check

  step "Links"
  local entry mode blob target
  while IFS= read -r -d '' entry; do
    read -r mode blob _ <<<"${entry%%$'\t'*}"
    [ "$mode" = 120000 ] || continue
    target=$(git cat-file blob "$blob")
    case $target in
      /* | [A-Za-z]:* | \\*) die "${entry#*$'\t'} is a link to an absolute path; a tracked link stays inside the repository" ;;
    esac
  done < <(git ls-files -s -z)

  step "Shell scripts"
  if command -v shellcheck >/dev/null; then
    shellcheck scripts/*.sh
  elif [ -n "${CI:-}" ]; then
    die "shellcheck is missing"
  else
    step "Skipped: shellcheck isn't installed here; CI runs it"
  fi
}

rust_checks
other_checks
step "Checks passed"
