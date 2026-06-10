#!/usr/bin/env bash
# PostToolUse hook: auto-format Rust after edits to *.rs (fast).
# Heavier checks (clippy/test) are workflow gates in the skills, not per-edit hooks.
path=$(python3 -c "import json,sys
try:
    d=json.load(sys.stdin); ti=d.get('tool_input',{})
    print(ti.get('file_path') or ti.get('path') or '')
except Exception:
    print('')" 2>/dev/null)
case "$path" in
  *.rs) cargo fmt --all >/dev/null 2>&1 || true ;;
esac
exit 0
