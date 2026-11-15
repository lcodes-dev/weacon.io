#!/usr/bin/env bash
# Runs Weacon on this machine for development: the server on http://localhost:8080, a TLS front
# for it on https://localhost:8443 that the core pins, and the core's daemon, with their data in
# .dev/; the core's goes in WEACON_STATE_DIR when it is set. The first run needs your email, for
# the server's owner. Ctrl-C stops all three.
# Usage: scripts/dev.sh [email]
set -euo pipefail

cd "$(dirname "$0")/.."

log() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

email=${1:-}
data=.dev/server
state=${WEACON_STATE_DIR:-.dev/state}
bin="${CARGO_TARGET_DIR:-target}/debug"

# A Unix socket's path holds at most 103 bytes on macOS.
case $state in
  /*) socket="$state/weacon.sock" ;;
  *) socket="$PWD/$state/weacon.sock" ;;
esac
[ "${#socket}" -le 103 ] || die "$socket is too long for a socket; set WEACON_STATE_DIR to a shorter folder"
new=false
if [ ! -e "$data/weacon.db" ]; then
  [ -n "$email" ] || die "the first run makes your account: scripts/dev.sh you@example.com"
  new=true
fi
mkdir -p "$data/outbox"

log "Building the web app, the server, the core and the TLS front"
npm --prefix apps/web run --silent build
cargo build --locked --quiet -p weacon-server -p weacon-core
cargo build --locked --quiet -p weacon-testing --bin dev-tls

server='' tls='' core=''
trap 'kill $server $tls $core 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM

"$bin/weacon-server" serve --db "$data/weacon.db" --outbox "$data/outbox" --secret "$data/secret" \
  >>.dev/server.log 2>&1 &
server=$!
"$bin/dev-tls" .dev/tls >>.dev/tls.log 2>&1 &
tls=$!
WEACON_STATE_DIR="$state" "$bin/weacon" daemon >>.dev/core.log 2>&1 &
core=$!

tries=0
until curl -fsS -o /dev/null http://localhost:8080/version 2>/dev/null; do
  tries=$((tries + 1))
  if [ "$tries" -ge 30 ] || ! kill -0 "$server" 2>/dev/null; then
    tail -n 5 .dev/server.log >&2
    die "the server didn't start; the end of .dev/server.log is above"
  fi
  sleep 1
done
kill -0 "$tls" 2>/dev/null || die "the TLS front didn't start; see .dev/tls.log"
kill -0 "$core" 2>/dev/null || die "the core's daemon didn't start; see .dev/core.log"
if [ "$new" = true ]; then
  "$bin/weacon-server" add-account --db "$data/weacon.db" --email "$email" >/dev/null 2>>.dev/server.log \
    || die "couldn't make the account for $email; see .dev/server.log"
  log "Made the account for $email, the server's owner"
fi

cat <<EOF

Weacon is running. Ctrl-C stops it.
  Web app:        http://localhost:8080
  Sign-in codes:  $data/outbox, one JSON file per code
  Pair the core:  WEACON_STATE_DIR=$state $bin/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem
  Logs:           .dev/server.log, .dev/tls.log, .dev/core.log and $state/logs/
EOF
wait "$server"
