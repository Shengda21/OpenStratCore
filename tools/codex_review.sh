#!/usr/bin/env bash
# Non-interactive Codex review of the working diff vs a base ref.
# Usage: tools/codex_review.sh [base_ref]   (default: main)
#
# 调用 codex 的方法（务必照此）：
#   codex exec --skip-git-repo-check "<prompt>" < /dev/null
#   非交互（别名 codex e）；已登录、模型 gpt-5.5；不要调交互式 TUI。
#   末尾 `�ɹ�: ����ֹ PID...` 乱码忽略（退出清理子进程的 GBK 提示）。Bash 超时设 ≥120000ms。
set -euo pipefail
BASE="${1:-main}"

if ! command -v codex >/dev/null 2>&1; then
  echo "error: 'codex' CLI not found. Sign in with your Codex subscription." >&2; exit 127
fi

# Collect the diff to review (works whether or not BASE/git exists).
DIFF="$(git diff "${BASE}" 2>/dev/null || true)"
[ -z "${DIFF}" ] && DIFF="$(git diff 2>/dev/null || true)"
[ -z "${DIFF}" ] && DIFF="(no git diff available — review the current working tree files relevant to the latest task)"

PROMPT="Review this change for a deterministic real-time wargame engine (project: OpenStratCore). Priorities, in order:
1. Determinism: randomness only via openstratcore_core::rng::Rng; no thread_rng, system time, or HashMap iteration-order reliance.
2. Fog-of-war: observations must not leak un-observed enemy info.
3. Contract-first: cross-layer struct changes must match schemas/*.json.
4. No panics in library code (no unwrap/expect/panic outside tests).
5. Rules-as-data: no hardcoded tunables that belong in config/rules.*.json; rule values must live in config/rules.*.json (rules-as-data), not hardcoded.
Output a concise list: [severity] file:line — issue — suggestion. If clean, say so explicitly.

--- DIFF ---
${DIFF}"

codex exec --skip-git-repo-check "${PROMPT}" < /dev/null
