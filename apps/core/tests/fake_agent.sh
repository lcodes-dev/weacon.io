#!/bin/sh
prompt="$* $(cat)"
case "$prompt" in
  *"Answer with JSON"*) echo '{"left_out": [], "works": [], "built": [{"title": "Prices", "summary": "How prices are formatted.", "files": ["src/price.ts"]}]}' ;;
  *) echo "Written by the program." ;;
esac
