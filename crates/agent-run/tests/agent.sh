#!/bin/sh
# Stands in for `claude` and `codex`: prints what the real one printed once, as recorded
# in this folder, with the folder it runs in put where the recording had its own.
here=$(dirname "$0")
prompt=$(cat)
case "$prompt" in
  *"fail"*) echo "not signed in" >&2; exit 1 ;;
esac
case "$prompt" in
  *"slow"*)
    # As the real programs do when interrupted: end the command, then go.
    sleep 30 &
    trap 'kill $!; touch cleaned-up; exit 130' INT
    echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Working."}]},"parent_tool_use_id":null}'
    wait ;;
esac
case "$*" in
  exec*) recording=codex.jsonl ;;
  *) recording=claude.jsonl ;;
esac
# What it was started with, for the test to read.
echo "$*" > started
sed "s|FOLDER|$PWD|g" "$here/$recording"
