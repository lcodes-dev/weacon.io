#!/usr/bin/env bash
# Builds a static release weacon for one target into dist/, checks it links only the OS and
# reports this commit, and prints its size. Run it on a machine that runs the target.
#   scripts/release.sh <target>
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

if [ "${1:-}" = install ]; then
  install_on "${2:-}" "${3:-}"
  exit
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
