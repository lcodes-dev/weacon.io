#!/usr/bin/env bash
# Runs every check CI runs, in the same order. Each must pass.
# `--mutants k/n` counts shards from 0, as cargo-mutants does.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/tools.sh
. scripts/tools.sh
use_mise
mkdir -p .dev

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

mutants=all
if [ $# -gt 0 ]; then
  if [ $# -ne 2 ] || [ "$1" != --mutants ]; then
    die "usage: scripts/check.sh [--mutants none|k/n]"
  fi
  [[ $2 =~ ^(none|[0-9]+/[0-9]+)$ ]] || die "--mutants takes none or a shard such as 0/4"
  [ "$2" = none ] || [ "${2%/*}" -lt "${2#*/}" ] || die "shard k of n needs k below n"
  mutants=$2
fi

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
  if rustup component list --installed 2>/dev/null | grep -q '^llvm-tools'; then
    need llvm-cov
    cargo llvm-cov --workspace --all-features --locked --fail-under-lines 90
  elif [ -n "${CI:-}" ]; then
    die "llvm-tools is missing, so the coverage floor can't be checked"
  else
    cargo test --workspace --all-features --locked --lib --bins --tests
    step "Coverage skipped: llvm-tools isn't installed here; CI checks the floor"
  fi
  cargo test --workspace --all-features --locked --doc
}

# The Rust changes against main, untracked files included.
changes() {
  local plain=(--no-ext-diff --no-color --src-prefix=a/ --dst-prefix=b/)
  git diff "${plain[@]}" "$1" -- '*.rs'
  git -c core.quotePath=false ls-files -z --others --exclude-standard -- '*.rs' |
    while IFS= read -r -d '' file; do
      git diff "${plain[@]}" --no-index /dev/null "$file" || true
    done
}

# Passes its arguments on to cargo mutants.
mutation_tests() {
  step "Mutation tests on the changed code"
  local base host
  base=$(git merge-base HEAD origin/main 2>/dev/null || true)
  if [ -z "$base" ]; then
    [ -z "${CI:-}" ] || die "origin/main isn't fetched, so the changed code can't be found"
    step "Skipped: origin/main isn't fetched here"
    return
  fi
  changes "$base" >.dev/changes.diff
  if [ ! -s .dev/changes.diff ]; then
    step "Skipped: no Rust changes against origin/main"
    return
  fi
  need mutants
  if [ -n "${CI:-}" ]; then
    # A mutant that allocates without end must fail as a test, not kill the runner.
    host=$(rustc -vV | sed -n 's/^host: //p' | tr 'a-z-' 'A-Z_')
    export "CARGO_TARGET_${host}_RUNNER=$PWD/scripts/limit-memory.sh"
  fi
  cargo mutants --in-diff .dev/changes.diff --output .dev --no-shuffle "$@"
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

case $mutants in
  all)
    rust_checks
    mutation_tests
    other_checks
    ;;
  none)
    rust_checks
    other_checks
    ;;
  *)
    # Round-robin spreads each file's mutants, and their slow tests, over every shard.
    mutation_tests --shard "$mutants" --sharding round-robin
    ;;
esac
step "Checks passed"
