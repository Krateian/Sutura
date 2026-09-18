#!/usr/bin/env bash
# pre-push security scan for the Sutura repo.
#
# Runs as a git pre-push hook: for every ref being pushed it scans the diff
# of the pushed range for secrets, absolute paths (outside tests/), and
# public IP addresses. Any real finding prints a clear warning and aborts
# the push (exit 1); a clean range exits 0.
#
# Test fixtures under tests/ are legitimate holders of synthetic paths, so
# absolute-path matches inside tests/* are skipped.
#
# Installed as .git/hooks/pre-push by install.sh / install-macos.sh.
set -u

RED=$'\033[1;31m'
GREEN=$'\033[1;32m'
YELLOW=$'\033[1;33m'
NC=$'\033[0m'

SEARCH_PATTERNS='ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[a-zA-Z0-9]{10,}|AKIA[0-9A-Z]{10,}|BEGIN (RSA |OPENSSH |EC |DSA )?PRIVATE KEY|Bearer [a-zA-Z0-9]'

scan_range() {
    local range="$1"
    local found=0

    # 1) secret/token patterns across the whole diff
    local secrets
    secrets=$(git diff "$range" 2>/dev/null | grep -nE "$SEARCH_PATTERNS" || true)
    if [ -n "$secrets" ]; then
        echo "${RED}SECRET/TOKEN PATTERN in $range:${NC}"
        echo "$secrets"
        found=1
    fi

    # 2) absolute paths outside tests/ (added lines, mapped per file)
    local files f hits
    files=$(git diff --diff-filter=ACMR --name-only "$range" 2>/dev/null || true)
    for f in $files; do
        case "$f" in
            tests/*) continue ;;  # test fixtures are legitimate
        esac
        hits=$(git diff "$range" -- "$f" 2>/dev/null \
            | grep -E '^\+' | grep -oE '/home/[a-zA-Z0-9_./-]+' || true)
        if [ -n "$hits" ]; then
            echo "${RED}ABSOLUTE PATH (outside tests/) in $f:${NC}"
            echo "$hits"
            found=1
        fi
    done

    # 3) public IPv4 addresses (private/local ranges are allowed)
    local ips
    ips=$(git diff "$range" 2>/dev/null | grep -E '^\+' \
        | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}' \
        | grep -vE '^(192\.168\.|10\.|127\.|0\.)' | sort -u || true)
    if [ -n "$ips" ]; then
        echo "${RED}PUBLIC IP ADDRESS in $range:${NC}"
        echo "$ips"
        found=1
    fi

    return "$found"
}

# pre-push stdin: one "<local ref> <local sha> <remote ref> <remote sha>" per ref
ZERO='0000000000000000000000000000000000000000'
overall=0
scanned=0
while read -r local_ref local_sha remote_ref remote_sha; do
    [ -z "$local_ref" ] && continue
    if [ "$remote_sha" = "$ZERO" ]; then
        # new branch: scan from the branch root commit
        base=$(git rev-list --max-parents=0 "$local_sha" 2>/dev/null | tail -1)
        range="$base..$local_sha"
    else
        range="$remote_sha..$local_sha"
    fi
    scanned=$((scanned + 1))
    scan_range "$range" || overall=1
done

if [ "$scanned" -eq 0 ]; then
    echo "${YELLOW}pre-push security scan: no refs to scan.${NC}"
    exit 0
fi

if [ "$overall" -eq 0 ]; then
    echo "${GREEN}pre-push security scan: clean ($scanned ref(s) checked).${NC}"
    exit 0
fi
echo "${RED}pre-push security scan: BLOCKED - fix the findings above and try again.${NC}"
exit 1