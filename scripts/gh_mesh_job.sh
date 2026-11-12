#!/usr/bin/env bash
# Local trigger/watch/fetch wrapper; never prints corpus paths.
set -euo pipefail
WF="${WF:-mesh-job.yml}"
base="${1:?base_ref}"; cand="${2:-}"; sets="${3:-samples40,oracle36}"
mode="${4:-quality_ab}"; envc="${5:-}"
args=(-f base_ref="$base" -f sets="$sets" -f mode="$mode")
[ -n "$cand" ] && args+=(-f candidate_ref="$cand")
[ -n "$envc" ] && args+=(-f env_candidate="$envc")
gh workflow run "$WF" "${args[@]}"
sleep 3
id=$(gh run list --workflow "$WF" --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$id" --exit-status
d=$(mktemp -d); gh run download "$id" -n aggregate -D "$d" 2>/dev/null || true
cat "$d/board.md" 2>/dev/null || true
