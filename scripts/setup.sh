#!/usr/bin/env bash
# Installs what a fresh machine, cloud session or CI runner needs to work on Weacon. Safe to run again.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/tools.sh
. scripts/tools.sh

# The Claude Code SessionStart hook passes `--claude-session-start`: no check tools, which take minutes
# to build, and no Node packages.
mode=all
if [ "${1:-}" = --claude-session-start ]; then
  [ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
  mode=session_start
fi

# The tools scripts/check.sh runs, at the versions CI uses.
tools=(deny@0.20.2 machete@0.9.2)

log() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Runs a command with its output in .dev/setup.log; when it fails, shows the log's end and stops.
run_logged() {
  local what=$1
  shift
  "$@" >>.dev/setup.log 2>&1 || {
    tail -n 30 .dev/setup.log >&2
    die "$what failed; the end of its output is above"
  }
}

mkdir -p .dev
: >.dev/setup.log
# mise installs the pinned Node and Rust. Without it, the installed ones must be exactly those.
install_mise="install mise from https://mise.jdx.dev, then run this again"
if [ -n "$mise" ]; then
  pinned=(rust)
  [ "$mode" != all ] || pinned+=(node)
  log "Installing the pinned tools with mise"
  run_logged "mise install" run_mise install "${pinned[@]}"
  use_mise
fi

toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
[ -n "$toolchain" ] || die "rust-toolchain.toml names no channel"
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$cargo_bin:$PATH"

rust_version() {
  rustup run "$1" rustc --version 2>/dev/null | cut -d' ' -f2 || true
}

ensure_toolchain() {
  command -v rustup >/dev/null || die "$install_mise"
  local toolchains alias stable host
  toolchains="${RUSTUP_HOME:-$HOME/.rustup}/toolchains"
  if [ "$(rust_version "$toolchain")" = "$toolchain" ]; then
    return
  fi
  # A stale alias, left by the fallback below after stable moved on, is replaced.
  for alias in "$toolchains/$toolchain"-*; do
    if [ -L "$alias" ]; then
      rm "$alias"
    fi
  done
  log "Installing Rust $toolchain"
  if rustup toolchain install "$toolchain" --profile minimal --component rustfmt,clippy >>.dev/setup.log 2>&1; then
    return
  fi
  # Cloud sessions can't reach static.rust-lang.org and have no mise, but come with a stable
  # toolchain. When that is the pinned version, it serves under the pinned name.
  stable=$(rust_version stable)
  [ "$stable" = "$toolchain" ] || die "can't download Rust $toolchain, and the installed stable is ${stable:-missing}"
  host=$(rustup run stable rustc -vV | sed -n 's/^host: //p')
  [ ! -e "$toolchains/$toolchain-$host" ] || die "$toolchains/$toolchain-$host is a broken toolchain; remove it and run this again"
  ln -s "stable-$host" "$toolchains/$toolchain-$host"
}

ensure_tools() {
  local tool name version
  for tool in "${tools[@]}"; do
    name=${tool%@*}
    version=${tool#*@}
    if cargo "$name" --version 2>/dev/null | grep -qw -- "$version"; then
      continue
    fi
    log "Building cargo-$name $version (a few minutes, once)"
    run_logged "cargo install cargo-$name" cargo install --locked --quiet "cargo-$name@$version"
  done
}

fetch_dependencies() {
  log "Fetching dependencies"
  run_logged "cargo fetch" cargo fetch --locked
}

ensure_node() {
  local want have
  want=$(cat .node-version)
  have=$(node --version 2>/dev/null || true)
  [ "$have" = "v$want" ] || die "Weacon's TypeScript needs Node $want, and the installed one is ${have:-missing}; $install_mise"
  local app
  for app in extension web; do
    log "Installing the $app's packages"
    run_logged "npm ci" npm ci --prefix "apps/$app" --include=dev --no-audit --no-fund
  done
}

# The web app's end-to-end tests drive Chromium. Playwright skips the download when the browser it
# pins is already in PLAYWRIGHT_BROWSERS_PATH, as in Claude Code cloud sessions.
ensure_chromium() {
  local install=(node apps/web/node_modules/playwright-core/cli.js install)
  [ -z "${CI:-}" ] || install+=(--with-deps)
  log "Installing Chromium for the web app's tests"
  run_logged "the Chromium install" "${install[@]}" chromium
}

# Later steps in a Claude Code session or a GitHub Actions job find the same tools.
persist_path() {
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    printf "export PATH=\"%s:\$PATH\"\n" "$cargo_bin" >>"$CLAUDE_ENV_FILE"
  fi
  if [ -n "${GITHUB_PATH:-}" ]; then
    printf '%s\n' "$cargo_bin" >>"$GITHUB_PATH"
  fi
}

ensure_toolchain
fetch_dependencies
if [ "$mode" = all ]; then
  ensure_tools
fi
persist_path
# Last, so a Node mismatch can't keep the Rust tools off PATH.
if [ "$mode" = all ]; then
  ensure_node
  ensure_chromium
fi
echo "Weacon setup done: $(rustup run "$toolchain" rustc --version). Run scripts/check.sh before committing."
