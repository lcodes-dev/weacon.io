#!/bin/sh
# Stands in for `claude` and `codex`: prints what the real one printed once, as recorded
# in this folder, with the folder it runs in put where the recording had its own.
here=$(dirname "$0")
prompt=$(cat)
case "$*" in
  exec*) recording=codex.jsonl ;;
  *) recording=claude.jsonl ;;
esac
# What it was started with, for the test to read.
echo "$*" > started
echo "$npm_config_cache" > cache
case "$prompt" in
  *"fail"*) echo "not signed in" >&2; exit 1 ;;
esac
# Each sandbox says in a command's output, in its own words, which host it refused.
case "$recording$prompt" in
  claude*"needs the network"*)
    printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","content":"npm error\n<sandbox_violations>\ndeny network-outbound registry.npmjs.org:443 (user denied)\n</sandbox_violations>"}]},"parent_tool_use_id":null}'
    echo '{"type":"result","session_id":"refused","is_error":false,"result":"Done."}'
    exit ;;
  codex*"needs the network"*)
    echo '{"type":"thread.started","thread_id":"refused"}'
    printf '%s\n' '{"type":"item.completed","item":{"type":"command_execution","command":"npm install","aggregated_output":"npm error\nNetwork access to \"registry.npmjs.org\" was blocked: domain is not on the allowlist for the current sandbox mode.\n","status":"failed"}}'
    echo '{"type":"turn.completed"}'
    exit ;;
esac
case "$prompt" in
  *"slow"*)
    # As the real programs do when interrupted: end the command, then go.
    sleep 30 &
    trap 'kill $!; touch cleaned-up; exit 130' INT
    echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Working."}]},"parent_tool_use_id":null}'
    wait ;;
esac
sed "s|FOLDER|$PWD|g" "$here/$recording"
