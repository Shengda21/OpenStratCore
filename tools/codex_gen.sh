#!/usr/bin/env bash
# Delegate a bounded generation/refactor task to Codex (uses your Codex subscription).
#
# 调用 codex 的方法（务必照此）：
#   用 Bash 执行： codex exec --skip-git-repo-check "<任务prompt>" < /dev/null
#   - 当前可能在非 git 目录，必须加 --skip-git-repo-check
#   - < /dev/null 避免它卡在读 stdin
#   - 这是非交互模式（别名 codex e），已登录、模型 gpt-5.5，直接返回结果即可用；交互式 TUI 不要调
#   - 输出末尾若有 `�ɹ�: ����ֹ PID...` 乱码，是退出时清理子进程的 GBK 提示，忽略即可
#   - 调用方（Claude Code 的 Bash 工具）超时请设到 120000ms 以上
#
# Usage:
#   tools/codex_gen.sh "task description"
#   tools/codex_gen.sh -o out.txt "task description"   # also tee output to a file
set -euo pipefail
OUT=""
while getopts "o:" opt; do
  case $opt in
    o) OUT="$OPTARG" ;;
    *) echo "usage: $0 [-o file] \"task\"" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
TASK="${1:?provide a task description}"

if ! command -v codex >/dev/null 2>&1; then
  echo "error: 'codex' CLI not found. Sign in with your Codex subscription." >&2; exit 127
fi

PROMPT="${TASK}

Follow AGENTS.md and CLAUDE.md. Keep changes minimal; do not alter public APIs without flagging.
Numbers belong in config/ (rules-as-data); ground truth for any rule value is the in-repo config/tables/*.json (rules-as-data).
Output must pass: cargo build, cargo clippy -D warnings, cargo test, and make validate."

run() { codex exec --skip-git-repo-check "${PROMPT}" < /dev/null; }
if [ -n "$OUT" ]; then run | tee "$OUT"; else run; fi
