# shellcheck shell=bash
# Sourced by the other scripts from the repository's root. Where mise is installed, `use_mise` puts
# the Node and Rust it pins here first on PATH, so no script depends on a shell that activated mise.

mise=$(command -v mise || true)
if [ -z "$mise" ] && [ -x "$HOME/.local/bin/mise" ]; then
  mise=$HOME/.local/bin/mise
fi

# These scripts already run this repository's code, so their own mise calls trust its mise.toml.
run_mise() {
  MISE_TRUSTED_CONFIG_PATHS=$PWD "$mise" "$@"
}

use_mise() {
  [ -n "$mise" ] || return 0
  local env
  env=$(run_mise env --shell bash) || {
    printf "error: mise couldn't list this repository's tools; run mise install\n" >&2
    exit 1
  }
  eval "$env"
}
