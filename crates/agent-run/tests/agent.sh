#!/bin/sh
# Stands in for `claude` and `codex`: prints what the real one printed once, as recorded
# in this folder, with the folder it runs in put where the recording had its own.
here=$(dirname "$0")
case "$*" in
  # Codex reads its prompt to the end of its input; Claude Code reads a line, and keeps its input open.
  exec*) recording=codex.jsonl; prompt=$(cat) ;;
  *) recording=claude.jsonl; read -r prompt ;;
esac
# What it was started with, for the test to read.
echo "$*" > started
echo "$npm_config_cache" > cache
case "$prompt" in
  *"fail"*) echo "not signed in" >&2; exit 1 ;;
esac
case "$recording$prompt" in
  claude*"needs the network"*)
    # Asks for something nobody can allow, then for a host, and waits for each answer.
    echo '{"type":"control_request","request_id":"ask-1","request":{"subtype":"can_use_tool","tool_name":"WebFetch","input":{"url":"https://example.com"}}}'
    read -r answer; echo "$answer" > answers
    echo '{"type":"control_request","request_id":"ask-2","request":{"subtype":"can_use_tool","tool_name":"SandboxNetworkAccess","input":{"host":"registry.npmjs.org"}}}'
    read -r answer; echo "$answer" >> answers
    echo '{"type":"result","session_id":"asked","is_error":false,"result":"Done."}'
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
