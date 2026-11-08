#!/usr/bin/env bash
# Installs what a fresh machine, cloud session or CI runner needs to work on Weacon. Safe to run again.
set -euo pipefail

cd "$(dirname "$0")/.."

# The Claude Code SessionStart hook passes this flag. It gets the toolchain and the dependencies but
# leaves the check tools, which take minutes to build, and the extension's Node packages to a plain
# `scripts/setup.sh`.
session_start=false
if [ "${1:-}" = "--claude-session-start" ]; then
  [ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
  session_start=true
fi

# The tools scripts/check.sh runs, at the versions CI uses.
tools=(deny@0.20.2 machete@0.9.2 llvm-cov@0.9.1 mutants@27.1.0)

log() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
[ -n "$toolchain" ] || die "rust-toolchain.toml names no channel"
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
export PATH="$cargo_bin:$PATH"
mkdir -p .dev

rust_version() {
  rustup run "$1" rustc --version 2>/dev/null | cut -d' ' -f2 || true
}

ensure_toolchain() {
  command -v rustup >/dev/null || die "install rustup from https://rustup.rs, then run this again"
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
  # Cloud sessions can't reach static.rust-lang.org but come with a stable toolchain. When that is the
  # pinned version, it serves under the pinned name.
  stable=$(rust_version stable)
  [ "$stable" = "$toolchain" ] || die "can't download Rust $toolchain, and the installed stable is ${stable:-missing}"
  host=$(rustup run stable rustc -vV | sed -n 's/^host: //p')
  [ ! -e "$toolchains/$toolchain-$host" ] || die "$toolchains/$toolchain-$host is a broken toolchain; remove it and run this again"
  ln -s "stable-$host" "$toolchains/$toolchain-$host"
}

# Coverage needs llvm-tools. Where it can't be downloaded, scripts/check.sh runs the tests without
# coverage and CI enforces the floor.
ensure_llvm_tools() {
  rustup component add llvm-tools --toolchain "$toolchain" >>.dev/setup.log 2>&1 \
    || log "Couldn't install llvm-tools; coverage will only be measured in CI"
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
    cargo install --locked --quiet "cargo-$name@$version" >>.dev/setup.log 2>&1 || {
      tail -n 30 .dev/setup.log >&2
      die "cargo install cargo-$name failed; the end of its output is above"
    }
  done
}

fetch_dependencies() {
  log "Fetching dependencies"
  cargo fetch --locked >>.dev/setup.log 2>&1 || {
    tail -n 30 .dev/setup.log >&2
    die "cargo fetch failed; the end of its output is above"
  }
}

ensure_node() {
  local want have
  want=$(cat .node-version)
  have=$(node --version 2>/dev/null || true)
  [ "$have" = "v$want" ] || die "Weacon's TypeScript needs Node $want, and the installed one is ${have:-missing}; install it, then run this again"
  log "Installing the extension's packages"
  npm ci --prefix apps/extension --include=dev --no-audit --no-fund >>.dev/setup.log 2>&1 || {
    tail -n 30 .dev/setup.log >&2
    die "npm ci failed; the end of its output is above"
  }
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

: >.dev/setup.log
ensure_toolchain
fetch_dependencies
if [ "$session_start" = false ]; then
  ensure_llvm_tools
  ensure_tools
fi
persist_path
# Last, so a Node mismatch can't keep the Rust tools off PATH.
if [ "$session_start" = false ]; then
  ensure_node
fi
echo "Weacon setup done: $(rustup run "$toolchain" rustc --version). Run scripts/check.sh before committing."
