#!/usr/bin/env bash
# Runs Weacon on this machine for development: the server on http://localhost:8080 and the
# core's daemon, with their data in .dev/; the core's goes in WEACON_STATE_DIR when it is set.
# The first run needs your email, for the server's owner. Ctrl-C stops both.
# Usage: scripts/dev.sh [email]
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/tools.sh
. scripts/tools.sh
use_mise

log() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# The string field named $1 of the JSON record $2, still escaped.
field() {
  sed -E "s/.*\"$1\":\"(([^\"\\\\]|\\\\.)*)\".*/\\1/" <<<"$2"
}

# Shows each mail the server prints as its log lines go by.
show_mails() {
  local line
  while IFS= read -r line; do
    case $line in
      *'"msg":"mail printed"'*)
        printf '\nMail to %s: %s\n  %s\n\n' "$(field to "$line")" "$(field subject "$line")" \
          "$(field text "$line")"
        ;;
    esac
  done
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
if [ ! -e "$data/weacon.db" ] && [ -z "$email" ]; then
  die "the first run makes your account: scripts/dev.sh you@example.com"
fi
[ apps/web/node_modules/.package-lock.json -nt apps/web/package-lock.json ] \
  || die "the web app's packages are missing or older than its package-lock.json; run scripts/setup.sh"
mkdir -p "$data"

log "Building the web app, the server and the core"
npm --prefix apps/web run --silent build
cargo build --locked --quiet -p weacon-server -p weacon-core

server='' core=''
trap 'kill $server $core 2>/dev/null || true; wait' EXIT
trap 'exit 130' INT TERM

"$bin/weacon-server" serve --db "$data/weacon.db" --secret "$data/secret" \
  > >(trap '' INT; tee -a .dev/server.log | show_mails) 2>&1 &
server=$!
WEACON_STATE_DIR="$state" "$bin/weacon" daemon >>.dev/core.log 2>&1 &
core=$!

tries=0
until curl -fsS --max-time 2 -o /dev/null http://localhost:8080/version 2>/dev/null; do
  tries=$((tries + 1))
  if [ "$tries" -ge 30 ] || ! kill -0 "$server" 2>/dev/null; then
    tail -n 5 .dev/server.log >&2
    die "the server didn't start; the end of .dev/server.log is above"
  fi
  sleep 1
done
kill -0 "$server" 2>/dev/null || die "the server didn't start; is another one on port 8080?"
kill -0 "$core" 2>/dev/null || die "the core's daemon didn't start; see .dev/core.log"
if [ -n "$email" ]; then
  if answer=$("$bin/weacon-server" add-account --db "$data/weacon.db" --email "$email" 2>&1 >/dev/null); then
    log "Made the account for $email"
  else
    case $answer in
      *'"db.exists"'*) ;;
      *)
        printf '%s\n' "$answer" >>.dev/server.log
        die "couldn't make the account for $email; see .dev/server.log"
        ;;
    esac
  fi
fi

cat <<EOF

Weacon is running. Ctrl-C stops it.
  Web app:        http://localhost:8080
  Sign-in codes:  shown here, in each mail the server prints
  Pair the core:  WEACON_STATE_DIR=$state $bin/weacon pair http://127.0.0.1:8080
  Logs:           .dev/server.log, .dev/core.log and $state/logs/
EOF
wait "$server"
