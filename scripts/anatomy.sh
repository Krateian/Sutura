#!/usr/bin/env bash
# Anatomy pass: static dead-code detection (vulture) cross-checked against
# dynamic execution coverage (coverage.py) for the ``sutura/`` package.
#
#   scripts/anatomy.sh [<python>]
#
# Read-only: it never edits or deletes source. It runs steps 2-4 of the
# "anatomy" brief and writes all artefacts under one output directory:
#
#   vulture.txt             raw vulture findings (min confidence 60)
#   vulture_whitelisted.txt findings minus scripts/vulture_whitelist.py
#   coverage-summary.txt    per-file coverage percentages
#   coverage.json           machine-readable coverage (per function/class)
#   htmlcov/                HTML coverage report
#   dead_code_candidates.txt functions/classes vulture flags AND coverage
#                           never executed -- the strongest dead-code
#                           candidates. Nothing is deleted; this is a list.
#
# Environment:
#   SUTURA_ANATOMY_OUT        output directory (default /tmp/sutura-anatomy)
#   SUTURA_ANATOMY_MIN_CONF   vulture min confidence (default 60)
#
# The interpreter defaults to the macOS conda env used for local work; pass
# an explicit path otherwise. It must have vulture, coverage, numpy,
# pymeshlab and trimesh available (the sutura-env / venv).
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PY="${1:-/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python}"
OUT="${SUTURA_ANATOMY_OUT:-/tmp/sutura-anatomy}"
MIN_CONF="${SUTURA_ANATOMY_MIN_CONF:-60}"
WHITELIST="$REPO/scripts/vulture_whitelist.py"

mkdir -p "$OUT"
cd "$REPO"

# Keep the run off the user's real cache/data directories.
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$OUT/cache}"
export SUTURA_DATA_DIR="${SUTURA_DATA_DIR:-$OUT/data}"

echo "== step 2: vulture (min-confidence $MIN_CONF) =="
# sutura/ is scanned directly; the test suite lives in the repo-root tests/,
# so it is never in scope and needs no exclude pattern.
"$PY" -m vulture sutura/ --min-confidence "$MIN_CONF" --sort-by-size \
    > "$OUT/vulture.txt" 2>/dev/null || true
"$PY" -m vulture sutura/ "$WHITELIST" --min-confidence "$MIN_CONF" --sort-by-size \
    > "$OUT/vulture_whitelisted.txt" 2>/dev/null || true
echo "  $(wc -l < "$OUT/vulture.txt") findings raw, $(wc -l < "$OUT/vulture_whitelisted.txt") after whitelist"

echo "== step 3: coverage =="
if [ "${SUTURA_ANATOMY_SKIP_COVERAGE:-0}" = "1" ]; then
    echo "  skipped (SUTURA_ANATOMY_SKIP_COVERAGE=1); reusing $OUT/coverage.json"
else
    rm -f .coverage .coverage.* 2>/dev/null || true
    "$PY" -m coverage run --source=sutura sutura/self_check.py
    for t in test_real_world_corpus test_deep_repair test_multiobject_escalation test_methods; do
        echo "  coverage run: tests/$t.py"
        "$PY" -m coverage run -p --source=sutura "tests/$t.py" || true
    done
    "$PY" -m coverage combine
    "$PY" -m coverage report --sort=cover > "$OUT/coverage-summary.txt"
    "$PY" -m coverage json -o "$OUT/coverage.json"
    "$PY" -m coverage html -d "$OUT/htmlcov"
    rm -f .coverage .coverage.* 2>/dev/null || true
fi

echo "== step 4: cross-check (vulture flags AND coverage never executed) =="
"$PY" - "$OUT" <<'PY'
import json
import os
import re
import sys

out = sys.argv[1]
with open(os.path.join(out, 'coverage.json')) as fh:
    cov = json.load(fh)

# path -> name -> executed (bool), for functions and classes alike.
# Coverage's JSON reports methods/classes under their plain name within the
# file, so a flat name map per file is sufficient for matching.
executed = {}
for path, info in cov.get('files', {}).items():
    key = path.replace(os.sep, '/')
    names = {}
    for name, fn in info.get('functions', {}).items():
        names[name] = bool(fn.get('executed_lines'))
    for name, cls in info.get('classes', {}).items():
        names[name] = bool(cls.get('executed_lines'))
    executed[key] = names

# Only definition-like vulture findings can be attributed to a coverage
# function/class; bare variables/imports are reported but not cross-checked.
finder = re.compile(
    r"^(?P<file>[^:]+):(?P<line>\d+): "
    r"(?:unused|unreachable|unsatisfiable) "
    r"(?P<kind>function|method|class|property) '(?P<name>[^']+)'"
)
rows = []
with open(os.path.join(out, 'vulture_whitelisted.txt')) as fh:
    for line in fh:
        m = finder.match(line.strip())
        if not m:
            continue
        path = m.group('file').replace(os.sep, '/')
        name = m.group('name')
        seen = executed.get(path, {})
        if name not in seen:
            # Not recorded by coverage at all: the defining module was never
            # imported during the run, i.e. never executed either.
            rows.append((path, int(m.group('line')), m.group('kind'),
                         name, 'not measured'))
        elif not seen[name]:
            rows.append((path, int(m.group('line')), m.group('kind'),
                         name, 'never executed'))

rows.sort(key=lambda r: (r[0], r[1]))
dest = os.path.join(out, 'dead_code_candidates.txt')
with open(dest, 'w') as fh:
    fh.write('# Strongest dead-code candidates: vulture flags these AND the\n')
    fh.write('# coverage run never executed them. Listed only; nothing deleted.\n')
    fh.write('# A candidate is a starting point, not a verdict: confirm with\n')
    fh.write('# git grep and an import walk before deleting (sutura-dev skill).\n')
    for path, line, kind, name, why in rows:
        fh.write('%s:%d: unused %s %s (%s)\n' % (path, line, kind, name, why))
    if not rows:
        fh.write('# (none)\n')
print('  %d strongest candidates -> %s' % (len(rows), dest))
PY

echo "done: artefacts under $OUT"
