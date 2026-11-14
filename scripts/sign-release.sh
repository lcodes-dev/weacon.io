#!/usr/bin/env bash
# Signs a draft release's manifest with a minisign secret key, on the machine that holds it, and
# uploads it with its signature to the draft. Needs gh, signed in to an account that may edit
# the repository's releases, minisign and jq. minisign asks for the key's password.
#   scripts/sign-release.sh <tag> <secret key file>
set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

tag=${1:-}
key=${2:-}
if ! [[ $tag =~ ^v(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$ ]] \
  || ! [ -f "$key" ]; then
  die "usage: scripts/sign-release.sh <vX.Y.Z> <secret key file>"
fi
# Found before the cd, so a relative path still works.
key=$(cd "$(dirname "$key")" && pwd)/$(basename "$key")
cd "$(dirname "$0")/.."
for tool in gh minisign jq sha256sum; do
  command -v "$tool" >/dev/null || die "$tool is missing"
done
# The public keys every build trusts, as the core embeds them.
keys=$(sed -n '/const TRUSTED/,/];/s/.*"\(RW[A-Za-z0-9+/]*\)".*/\1/p' apps/core/src/update/verify.rs)
[ -n "$keys" ] || die "the trusted keys weren't found"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/builds" "$work/signed"
gh release download "$tag" --dir "$work" --pattern manifest.json
manifest="$work/manifest.json"
jq -e --arg version "${tag#v}" '.version == $version' "$manifest" >/dev/null \
  || die "the draft's manifest isn't for $tag"

# Each build must be the one the manifest names.
while IFS=$'\t' read -r url size sha256; do
  name=${url##*/}
  [[ $name =~ ^weacon-[A-Za-z0-9_-]+(\.exe)?$ ]] || die "the manifest names an odd build: $name"
  gh release download "$tag" --dir "$work/builds" --pattern "$name"
  if [ "$(wc -c <"$work/builds/$name" | tr -d ' ')" != "$size" ] \
    || [ "$(sha256sum "$work/builds/$name" | cut -d ' ' -f 1)" != "$sha256" ]; then
    die "$name doesn't match the manifest"
  fi
done < <(jq -r '.builds[] | [.url, .size, .sha256] | @tsv' "$manifest")

# Dated when it's signed, so a core can tell the newest manifest from one held back.
signed="$work/signed/manifest.json"
jq -c --argjson now "$(date +%s)" '.published_at = $now' "$manifest" >"$signed"
minisign -S -s "$key" -m "$signed" -x "$signed.minisig" -t "weacon ${tag#v}"

trusted=
for public in $keys; do
  if minisign -V -q -P "$public" -m "$signed" -x "$signed.minisig" 2>/dev/null; then
    trusted=1
  fi
done
[ -n "$trusted" ] || die "the signature isn't by a key the core trusts"

gh release upload "$tag" "$signed" "$signed.minisig" --clobber
printf 'Signed %s. Publish the release, then put manifest.json and manifest.json.minisig at\n' "$tag"
printf 'https://weacon.io/releases/stable/.\n'
