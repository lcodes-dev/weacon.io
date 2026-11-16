#!/usr/bin/env bash
# Builds a static release weacon for one target into dist/, checks it links only the OS and
# reports this commit, and prints its size. Run it on a machine that runs the target.
#   scripts/release.sh <target>
# Or builds the web app and a static release weacon-server, with the web app in it, for a Linux
# target, and checks it reports this commit and serves this web app:
#   scripts/release.sh server <target>
# Or copies a Linux build to a host over SSH and installs it there as a systemd user service:
#   scripts/release.sh install <[user@]host> <target>
set -euo pipefail

cd "$(dirname "$0")/.."

# The targets every release builds the core for; the release workflow's matrix lists them too.
# Only the Linux ones build the server.
LINUX_TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
CORE_TARGETS=("${LINUX_TARGETS[@]}" aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc)
LINUX_CHOICE=${LINUX_TARGETS[*]}
LINUX_CHOICE=${LINUX_CHOICE// / or }

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Whether $1 is one of the rest of the arguments.
one_of() {
  local item=$1 each
  shift
  for each; do
    [ "$each" = "$item" ] && return
  done
  return 1
}

# What a Windows build's file name ends in, and nothing for the others.
exe_suffix() {
  [[ $1 != *-windows-* ]] || printf '.exe'
}

install_on() {
  local host=${1:-} target=${2:-} arch
  # Checked so ssh and scp can't read the host as an option or a path.
  [[ $host =~ ^([A-Za-z0-9_][A-Za-z0-9_.-]*@)?[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
    || die "usage: scripts/release.sh install <[user@]host> <target>, with a plain host name"
  one_of "$target" "${LINUX_TARGETS[@]}" || die "install takes a Linux target: $LINUX_CHOICE"
  local binary="dist/weacon-$target"
  [ -f "$binary" ] || die "$binary is missing: run scripts/release.sh $target first"
  arch=$(ssh -- "$host" 'mkdir -p ~/.local/bin && uname -m') || die "couldn't reach $host"
  [ "$arch" = "${target%%-*}" ] || die "$host runs $arch, not ${target%%-*}"
  scp -q -- "$binary" "$host:.local/bin/weacon.new"
  # Renamed into place, so a running daemon keeps its binary until the restart.
  ssh -- "$host" 'chmod 755 ~/.local/bin/weacon.new && mv ~/.local/bin/weacon.new ~/.local/bin/weacon \
    && ~/.local/bin/weacon service install && ~/.local/bin/weacon --version'
}

# The web app's built files, as apps/server/build.rs embeds them.
web_files=(index.html app.js app.css icon.svg)

# Starts the server on a free port and checks it serves each built file of the web app.
check_serves() {
  local binary=$1 dir log address file route version
  dir=$(mktemp -d)
  log="$dir/log"
  "$binary" serve --listen 127.0.0.1:0 --db "$dir/db" --secret "$dir/secret" >"$log" 2>&1 &
  local pid=$!
  # shellcheck disable=SC2064 # The values are fixed now, on purpose.
  trap "kill $pid 2>/dev/null; wait $pid 2>/dev/null; rm -rf '$dir'" EXIT
  for _ in $(seq 100); do
    address=$(grep -F '"msg":"listening"' "$log" | grep -Eo '127\.0\.0\.1:[0-9]+' || true)
    if [ -n "$address" ] || ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 0.1
  done
  [ -n "$address" ] || die "$binary didn't start serving"
  version=$(curl -fsS "http://$address/version") || die "$binary doesn't answer at /version"
  grep -qF "\"$(git rev-parse HEAD)\"" <<<"$version" \
    || die "$binary doesn't report this commit at /version"
  for file in "${web_files[@]}"; do
    route=/$file
    [ "$file" = index.html ] && route=/
    curl -fsS "http://$address$route" | cmp -s - "apps/web/dist/$file" \
      || die "$binary doesn't serve this web app's $file"
  done
}

# Builds the core, or with `server` first the server, for a target into dist/, and checks it.
build() {
  local server='' bin=weacon package=weacon-core target exe changes binary links
  if [ "${1:-}" = server ]; then
    server=1 bin=weacon-server package=weacon-server
    shift
  fi

  target=${1:-}
  one_of "$target" "${CORE_TARGETS[@]}" \
    || die "usage: scripts/release.sh [server] <target>, for a target this script lists"
  exe=$(exe_suffix "$target")
  case "$target" in
    *-linux-musl) export "CC_${target//-/_}=musl-gcc" ;;
    *-windows-msvc) export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static" ;;
  esac
  [ -z "$server" ] || one_of "$target" "${LINUX_TARGETS[@]}" \
    || die "server takes a Linux target: $LINUX_CHOICE"

  changes=$(git status --porcelain --untracked-files=normal)
  [ -z "$changes" ] || die "the working tree has changes, so the binary wouldn't be this commit"

  rustup target add "$target" >/dev/null

  if [ -n "$server" ]; then
    rm -rf apps/web/dist
    npm --prefix apps/web run build >/dev/null
    for file in "${web_files[@]}"; do
      [ -s "apps/web/dist/$file" ] || die "the web app's build didn't make $file"
    done
  fi
  cargo build --release --locked --package "$package" --target "$target"

  mkdir -p dist
  binary="dist/$bin-$target$exe"
  cp "target/$target/release/$bin$exe" "$binary"

  case "$target" in
    *-linux-musl)
      file "$binary" | grep -Eq 'statically linked|static-pie linked' || die "$binary isn't static" ;;
    *-apple-darwin)
      links=$(otool -L "$binary")
      ! tail -n +2 <<<"$links" | grep -Ev '^[[:space:]]+/(usr/lib|System/Library)/' \
        || die "$binary links libraries macOS doesn't ship" ;;
    *-windows-msvc)
      ! grep -aqiE '(vcruntime|msvcp)[0-9_]+\.dll|api-ms-win-crt-' "$binary" \
        || die "$binary needs the Visual C++ runtime" ;;
  esac

  "$binary" --version | tr -d '\r' | grep -qF "($(git rev-parse HEAD))" \
    || die "$binary doesn't report this commit"
  [ -z "$server" ] || check_serves "$binary"

  printf '%s: %s bytes\n' "$binary" "$(wc -c <"$binary" | tr -d ' ')"
}

case "${1:-}" in
  install) install_on "${2:-}" "${3:-}" ;;
  *) build "$@" ;;
esac
