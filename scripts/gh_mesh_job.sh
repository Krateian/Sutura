#!/usr/bin/env bash
# Local trigger/watch/fetch wrapper; never prints corpus paths.
set -euo pipefail
WF="${WF:-mesh-job.yml}"
REPO="${REPO:-Krateian/Sutura}"
base="${1:?base_ref}"; cand="${2:-}"; sets="${3:-samples40@quality_ab,oracle36@quality_ab}"
mode="${4:-quality_ab}"; envc="${5:-}"
args=(-f base_ref="$base" -f sets="$sets" -f mode="$mode")
[ -n "$cand" ] && args+=(-f candidate_ref="$cand")
[ -n "$envc" ] && args+=(-f env_candidate="$envc")
gh workflow run "$WF" -R "$REPO" "${args[@]}"
sleep 5
id=$(gh run list -R "$REPO" --workflow "$WF" --limit 1 \
       --json databaseId --jq '.[0].databaseId')
gh run watch "$id" -R "$REPO" --exit-status || true
d=$(mktemp -d)
gh run download "$id" -R "$REPO" -n board -D "$d"
cat "$d/board.md"
echo "run: https://github.com/$REPO/actions/runs/$id"
