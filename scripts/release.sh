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

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

install_on() {
  local host=${1:-} target=${2:-} arch
  # Checked so ssh and scp can't read the host as an option or a path.
  [[ $host =~ ^([A-Za-z0-9_][A-Za-z0-9_.-]*@)?[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
    || die "usage: scripts/release.sh install <[user@]host> <target>, with a plain host name"
  case "$target" in
    x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) ;;
    *) die "install takes a Linux target: x86_64-unknown-linux-musl or aarch64-unknown-linux-musl" ;;
  esac
  local binary="dist/weacon-$target"
  [ -f "$binary" ] || die "$binary is missing: run scripts/release.sh $target first"
  arch=$(ssh -- "$host" 'mkdir -p ~/.local/bin && uname -m') || die "couldn't reach $host"
  [ "$arch" = "${target%%-*}" ] || die "$host runs $arch, not ${target%%-*}"
  scp -q -- "$binary" "$host:.local/bin/weacon.new"
  # Renamed into place, so a running daemon keeps its binary until the restart.
  ssh -- "$host" 'chmod 755 ~/.local/bin/weacon.new && mv ~/.local/bin/weacon.new ~/.local/bin/weacon \
    && ~/.local/bin/weacon service install && ~/.local/bin/weacon --version'
}

# Starts the server on a free port and checks it serves each built file of the web app.
check_serves() {
  local binary=$1 dir log address file route
  dir=$(mktemp -d)
  log="$dir/log"
  mkdir "$dir/outbox"
  "$binary" serve --listen 127.0.0.1:0 --db "$dir/db" --outbox "$dir/outbox" \
    --secret "$dir/secret" >"$log" 2>&1 &
  local pid=$!
  # shellcheck disable=SC2064 # The values are fixed now, on purpose.
  trap "kill $pid 2>/dev/null; rm -rf '$dir'" EXIT
  for _ in $(seq 100); do
    address=$(grep -F '"msg":"listening"' "$log" | grep -Eo '127\.0\.0\.1:[0-9]+' || true)
    if [ -n "$address" ] || ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 0.1
  done
  [ -n "$address" ] || die "$binary didn't start serving"
  curl -fsS "http://$address/version" | grep -qF "\"$(git rev-parse HEAD)\"" \
    || die "$binary doesn't report this commit at /version"
  for file in index.html app.js app.css; do
    route=/$file
    [ "$file" = index.html ] && route=/
    curl -fsS "http://$address$route" | cmp -s - "apps/web/dist/$file" \
      || die "$binary doesn't serve this web app's $file"
  done
}

if [ "${1:-}" = install ]; then
  install_on "${2:-}" "${3:-}"
  exit
fi

server=
if [ "${1:-}" = server ]; then
  server=1
  shift
  case "${1:-}" in
    *-linux-musl) ;;
    *) die "usage: scripts/release.sh server <target>, with a Linux target this script lists" ;;
  esac
fi

target=${1:-}
exe=
case "$target" in
  x86_64-apple-darwin | aarch64-apple-darwin) ;;
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) export "CC_${target//-/_}=musl-gcc" ;;
  x86_64-pc-windows-msvc)
    exe=.exe
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static" ;;
  *) die "usage: scripts/release.sh <target>, for a target this script lists" ;;
esac

changes=$(git status --porcelain --untracked-files=normal)
[ -z "$changes" ] || die "the working tree has changes, so the binary wouldn't be this commit"

rustup target add "$target" >/dev/null

if [ -n "$server" ]; then
  [ -d apps/web/node_modules ] || die "the web app's packages are missing: run scripts/setup.sh"
  rm -rf apps/web/dist
  npm --prefix apps/web run build >/dev/null
  for file in index.html app.js app.css; do
    [ -s "apps/web/dist/$file" ] || die "the web app's build didn't make $file"
  done
  grep -qF "$(git rev-parse HEAD)" apps/web/dist/app.js || die "the web app doesn't carry this commit"
  cargo build --release --locked --package weacon-server --target "$target"
  mkdir -p dist
  binary="dist/weacon-server-$target"
  cp "target/$target/release/weacon-server" "$binary"
  file "$binary" | grep -Eq 'statically linked|static-pie linked' || die "$binary isn't static"
  "$binary" --version | grep -qF "($(git rev-parse HEAD))" || die "$binary doesn't report this commit"
  check_serves "$binary"
  printf '%s: %s bytes\n' "$binary" "$(wc -c <"$binary" | tr -d ' ')"
  exit
fi

cargo build --release --locked --package weacon-core --target "$target"

mkdir -p dist
binary="dist/weacon-$target$exe"
cp "target/$target/release/weacon$exe" "$binary"

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

printf '%s: %s bytes\n' "$binary" "$(wc -c <"$binary" | tr -d ' ')"
