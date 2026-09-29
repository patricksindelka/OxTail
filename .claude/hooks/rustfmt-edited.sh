#!/usr/bin/env bash
# PostToolUse hook: format a Rust file right after an agent edits it.
# Never fails the tool call; formatting errors surface later in `cargo fmt --check`.
file=$(jq -r '.tool_input.file_path // empty' 2>/dev/null)
case "$file" in
  *.rs) [ -f "$file" ] && rustfmt --edition 2024 --quiet "$file" 2>/dev/null ;;
esac
exit 0
