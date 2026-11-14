#!/usr/bin/env bash
# Builds a static release weacon for one target into dist/, checks it links only the OS and
# reports this commit, and prints its size. Run it on a machine that runs the target.
#   scripts/release.sh <target>
# Or builds the web app and a static release weacon-server, with the web app in it, for a Linux
# target, and checks it reports this commit and serves this web app:
#   scripts/release.sh server <target>
# Or copies a Linux build to a host over SSH and installs it there as a systemd user service:
#   scripts/release.sh install <[user@]host> <target>
# Or writes the unsigned manifest of the core's builds in a folder, as published at a base URL:
#   scripts/release.sh manifest <tag> <folder> <https base URL>
# Or, on the machine with the release key, checks a draft release's manifest and builds against
# its tag, signs the manifest with minisign and uploads it with its signature to the draft:
#   scripts/release.sh sign <tag> <secret key file>
set -euo pipefail

caller=$PWD
cd "$(dirname "$0")/.."

# The targets every release builds the core for; the manifest names a build for each.
CORE_TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin
  x86_64-apple-darwin x86_64-pc-windows-msvc)

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

# The web app's built files, as apps/server/build.rs embeds them.
web_files=(index.html app.js app.css)

# Starts the server on a free port and checks it serves each built file of the web app.
check_serves() {
  local binary=$1 dir log address file route version
  dir=$(mktemp -d)
  log="$dir/log"
  mkdir "$dir/outbox"
  "$binary" serve --listen 127.0.0.1:0 --db "$dir/db" --outbox "$dir/outbox" \
    --secret "$dir/secret" >"$log" 2>&1 &
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

workspace_version() {
  sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml
}

# The file a target's build of the core is published as.
build_name() {
  case "$1" in
    *-windows-*) printf 'weacon-%s.exe' "$1" ;;
    *) printf 'weacon-%s' "$1" ;;
  esac
}

# A file's size and SHA-256 on one line; shasum, unlike sha256sum, comes with macOS too.
size_and_sha256() {
  local size sha256
  size=$(wc -c <"$1" | tr -d ' ')
  sha256=$(shasum -a 256 "$1" | cut -d ' ' -f 1)
  [[ $sha256 =~ ^[0-9a-f]{64}$ ]] || return 1
  printf '%s %s\n' "$size" "$sha256"
}

# Writes <folder>/manifest.json for the core's builds there. It is dated when it's signed.
write_manifest() {
  local tag=${1:-} dir=${2:-} base=${3:-} protocol commit target name size sha256 builds=()
  if [ "$tag" != "v$(workspace_version)" ] || ! [ -d "$dir" ] || [[ $base != https://* ]]; then
    die "usage: scripts/release.sh manifest v$(workspace_version) <folder> <https base URL>"
  fi
  protocol=$(sed -n 's/^pub const PROTOCOL: u32 = \([0-9]*\);$/\1/p' crates/protocol/src/message.rs)
  [ -n "$protocol" ] || die "the protocol version wasn't found"
  # As for the builds: WEACON_COMMIT when it isn't blank, else git's.
  commit=${WEACON_COMMIT:-}
  [ -n "$commit" ] || commit=$(git rev-parse HEAD) || die "the commit wasn't found; set WEACON_COMMIT"
  [[ $commit =~ ^[0-9a-f]{40}$ ]] || die "the commit isn't a full commit id"
  for target in "${CORE_TARGETS[@]}"; do
    name=$(build_name "$target")
    [ -f "$dir/$name" ] || die "$dir has no $name"
    read -r size sha256 < <(size_and_sha256 "$dir/$name") || die "$name couldn't be read"
    builds+=("$(jq -cn --arg target "$target" --arg url "${base%/}/$name" \
      --argjson size "$size" --arg sha256 "$sha256" \
      '{target: $target, url: $url, size: $size, sha256: $sha256}')")
  done
  printf '%s\n' "${builds[@]}" | jq -cs --arg version "${tag#v}" --arg commit "$commit" \
    --argjson protocol "$protocol" \
    '{version: $version, commit: $commit, published_at: 0, protocol: $protocol, builds: .}' \
    >"$dir/manifest.json"
  printf '%s: %s builds\n' "$dir/manifest.json" "${#builds[@]}"
}

# Signs the draft release of <tag> once its manifest and builds match the tag.
sign_release() {
  local tag=${1:-} key=${2:-} tool commit repo keys target name public trusted=
  [[ $key == /* ]] || key="$caller/$key"
  if ! [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || ! [ -f "$key" ]; then
    die "usage: scripts/release.sh sign <vX.Y.Z> <secret key file>"
  fi
  for tool in gh minisign jq shasum; do
    command -v "$tool" >/dev/null || die "$tool is missing"
  done
  commit=$(git rev-parse -q --verify "$tag^{commit}") || die "$tag isn't here: fetch the tags"
  repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
  # The keys the tag's own core trusts.
  keys=$(git show "$tag:apps/core/src/update/verify.rs" \
    | sed -n '/const TRUSTED/,/];/s/.*"\(RW[A-Za-z0-9+/]*\)".*/\1/p')
  [ -n "$keys" ] || die "the trusted keys weren't found at $tag"

  # Global, so the trap still sees it once this returns.
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  gh release download "$tag" --repo "$repo" --dir "$work" --pattern manifest.json
  jq -e --arg version "${tag#v}" --arg commit "$commit" \
    --arg base "https://github.com/$repo/releases/download/$tag/" \
    --argjson targets "$(printf '%s\n' "${CORE_TARGETS[@]}" | jq -R . | jq -cs .)" '
      .version == $version and .commit == $commit
      and ([.builds[].target] | sort) == ($targets | sort)
      and all(.builds[]; .url == $base + "weacon-" + .target
        + (if .target | test("-windows-") then ".exe" else "" end))' \
    "$work/manifest.json" >/dev/null || die "the draft's manifest doesn't describe $tag's builds"
  for target in "${CORE_TARGETS[@]}"; do
    name=$(build_name "$target")
    gh release download "$tag" --repo "$repo" --dir "$work" --pattern "$name"
    [ "$(size_and_sha256 "$work/$name")" = "$(jq -r --arg target "$target" \
      '.builds[] | select(.target == $target) | "\(.size) \(.sha256)"' "$work/manifest.json")" ] \
      || die "$name doesn't match the draft's manifest"
  done

  mkdir "$work/signed"
  jq -c --argjson now "$(date +%s)" '.published_at = $now' "$work/manifest.json" \
    >"$work/signed/manifest.json"
  # Prehashed only: the core refuses legacy signatures.
  minisign -S -H -s "$key" -m "$work/signed/manifest.json" -t "weacon ${tag#v}"
  for public in $keys; do
    if minisign -V -H -q -P "$public" -m "$work/signed/manifest.json" 2>/dev/null; then
      trusted=1
      break
    fi
  done
  [ -n "$trusted" ] || die "the signature isn't by a key $tag's core trusts"
  gh release upload "$tag" --repo "$repo" --clobber \
    "$work/signed/manifest.json" "$work/signed/manifest.json.minisig"
  printf 'Signed %s. Publish the draft, then put manifest.json and manifest.json.minisig at\n' "$tag"
  printf 'https://weacon.io/releases/stable/.\n'
}

case "${1:-}" in
  install)
    install_on "${2:-}" "${3:-}"
    exit ;;
  manifest)
    write_manifest "${2:-}" "${3:-}" "${4:-}"
    exit ;;
  sign)
    sign_release "${2:-}" "${3:-}"
    exit ;;
esac

server='' bin=weacon package=weacon-core
if [ "${1:-}" = server ]; then
  server=1 bin=weacon-server package=weacon-server
  shift
fi

target=${1:-}
exe=
case "$target" in
  x86_64-apple-darwin | aarch64-apple-darwin) ;;
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) export "CC_${target//-/_}=musl-gcc" ;;
  x86_64-pc-windows-msvc)
    exe=.exe
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static" ;;
  *) die "usage: scripts/release.sh [server] <target>, for a target this script lists" ;;
esac
[ -z "$server" ] || [[ $target == *-linux-musl ]] \
  || die "server takes a Linux target: x86_64-unknown-linux-musl or aarch64-unknown-linux-musl"

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
