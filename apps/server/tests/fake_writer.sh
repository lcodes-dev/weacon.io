#!/bin/sh
# Stands in for `claude` and `codex` in the tests: reads the prompt, prints an answer.
# `claude` gets its system prompt as an argument and `codex` on its input.
prompt="$* $(cat)"
case "$prompt" in
  *"Answer with JSON"*) echo '{"left_out": [], "works": [], "built": [{"title": "Prices", "summary": "How prices are formatted.", "files": ["src/price.ts"]}]}' ;;
  *) echo "Written by the program." ;;
esac
