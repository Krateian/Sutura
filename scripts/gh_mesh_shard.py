#!/usr/bin/env python3
"""One shard: run the verify harness with per-side repos/pythons and real keys.

Each row carries the baseline and the candidate metric record, so the board
can show the visible two-sided values next to the ``*_all_*`` values and the
SI-after pair that decides TRADE-SI.

A missing OrcaSlicer is not a missing row: the harness reports
``orca_number_of_parts=None`` / ``orca_manifold='unknown'`` and the judge falls
back to the component count, so Orca is reported but never required.
"""
import glob
import json
import os
import subprocess
import sys
import tempfile
import traceback

VERIFY = os.environ.get(
    'VERIFY_SCRIPT',
    'tools/.claude/skills/sutura-verify/scripts/verify_default_vs_optout.py')
SHAPE_REQUIRED = ('reload_holes', 'reload_non_manifold_edges', 'components',
                  'closed_shells', 'two_sided_max_mm', 'two_sided_p95_mm',
                  'leak_max_mm', 'leak_count', 'si_output')
SHAPE_KEYS = SHAPE_REQUIRED + (
    'orca_number_of_parts', 'orca_manifold', 'volume_error_rel',
    'two_sided_max_all_mm', 'two_sided_p95_all_mm',
    'i2o_max_all_mm', 'i2o_p95_all_mm', 'si_output_note', 'buried_note')
BYTE_KEYS = ('md5', 'ftetwild_adopted')
# The judge skips the SI criterion when BOTH sides lack exact SI for the face
# cap; such a row is judged on its other metrics, so si_output must not count
# as a missing metric here.
SI_FACE_CAP_NOTE = 'skipped (face cap)'
# determinism is a byte comparison of two independent runs of the same ref,
# with the stochastic fTetWild path excluded.
FLAGS = {'byte': ['--byte', '--deterministic'],
         'determinism': ['--byte', '--deterministic',
                         '--exclude-nondeterministic'],
         'quality_ab': ['--quality', '--no-visual'],
         'single_side': ['--quality', '--no-visual'],
         # bench115 has fTetWild-adopting meshes; an adopted result is
         # stochastic, so it is reported as SKIPPED-NONDET instead of being
         # compared, exactly as the determinism and core_clean sets do.
         'bench115': ['--quality', '--no-visual',
                      '--exclude-nondeterministic']}
QUALITY_MODES = ('quality_ab', 'single_side', 'bench115')


def resolve_mesh(corpus, rel):
    """Private-corpus path first, then the public checkout's own copy.

    The derived sets (``data/derived/stl/...``) live in the private corpus
    repo; ``samples40`` points at ``tests/real-world-samples``, which is
    already tracked in the public repo.  ``bench115`` lists bare file names and
    the chunk fetch extracts them to ``corpus/bench115/``.  Reading the public
    copy from the base checkout keeps those 40 files out of the private repo.
    """
    for root in (corpus, os.path.join(os.getcwd(), 'base'),
                 os.path.join(corpus, 'bench115')):
        p = os.path.join(root, rel)
        if os.path.exists(p):
            return p
    return os.path.join(corpus, rel)


def manifest(sets, set_name):
    """The set's manifest, tolerating the count suffix in the file name.

    The private corpus names its three-mesh nondeterminism set
    ``determinism3.txt``; the workflow set is called ``determinism``.  An
    exact name wins; otherwise a unique ``<name>*.txt`` match is used, and
    an ambiguous or missing name is an error rather than a silent empty run.
    """
    exact = os.path.join(sets, set_name + '.txt')
    if os.path.exists(exact):
        return exact
    matches = sorted(glob.glob(os.path.join(sets, set_name + '*.txt')))
    if len(matches) == 1:
        return matches[0]
    raise SystemExit('no unique manifest for set %r in %s (matches: %s)'
                     % (set_name, sets, [os.path.basename(m) for m in matches]))


def strict_watertight(rec):
    """September-style strict watertight: no holes AND no non-manifold edges.

    ``reload_holes`` / ``reload_non_manifold_edges`` are the harness's
    ``defects.detect()`` counts on the reloaded output, which is exactly the
    metric in docs/repair-benchmark-strict-watertight-2026-09.md.  None when
    either count is unavailable (the row must not read as a pass).
    """
    holes, nm = rec.get('reload_holes'), rec.get('reload_non_manifold_edges')
    if holes is None or nm is None:
        return None
    return bool(holes == 0 and nm == 0)


def main():
    set_name = os.environ['SET_NAME']
    shard = int(os.environ['SHARD'])
    shards = int(os.environ['SHARDS'])
    mode = os.environ['MODE']
    env_c = os.environ.get('ENV_CAND', '')
    env_b = os.environ.get('ENV_BASE', '')
    if env_b:
        raise SystemExit('ENV_BASE is not supported by the harness '
                         '(candidate-side env only)')
    quality = mode in QUALITY_MODES
    keys = SHAPE_KEYS if quality else BYTE_KEYS
    required = SHAPE_REQUIRED if quality else BYTE_KEYS
    corpus = os.path.join(os.getcwd(), 'corpus')
    sets = os.path.join(corpus, 'sets')
    files = [l.strip() for l in open(manifest(sets, set_name)) if l.strip()]
    # bench115 ships one manifest per packed chunk (bench115-chunk-<k>.txt,
    # staged as sets/bench115.txt), so the file list IS this shard's slice and
    # must not be sub-sliced again.  Every other set is sliced from its full
    # manifest here.
    if set_name != 'bench115':
        files = files[shard::shards]
    outdir = tempfile.mkdtemp(prefix='shard-')
    # determinism compares two independent runs of the SAME ref (the
    # candidate, or the base when no candidate is given), never base vs cand.
    a_repo, a_py = (('cand', 'cand-venv') if mode == 'determinism'
                    else ('base', 'base-venv'))
    cmd = [sys.executable, VERIFY] \
        + [resolve_mesh(corpus, f) for f in files] + FLAGS[mode] + [
            '--baseline-repo', os.path.abspath(a_repo),
            '--baseline-python', os.path.abspath(a_py + '/bin/python'),
            '--repo', os.path.abspath('cand'),
            '--python', os.path.abspath('cand-venv/bin/python'),
            '--out-dir', outdir]
    if env_c:
        cmd += ['--opt-out-env', env_c]
    if mode == 'determinism':
        # The baseline half is cached by input SHA + flags + repo head, so a
        # warmed cache would compare a run against itself.  A fresh cache keeps
        # the two runs independent, which is the point of the check.
        cmd += ['--cache-dir', os.path.join(outdir, 'cache')]
    r = subprocess.run(cmd, capture_output=True, text=True)
    rows = []
    err = None
    sp = os.path.join(outdir, 'summary.json')
    if os.path.exists(sp):
        for item in json.load(open(sp)):
            sides = {side: {k: (item.get(side) or {}).get(k) for k in keys}
                     for side in ('baseline', 'candidate')}
            # Both sides capped -> the judge deliberately skipped SI; it is not
            # a missing metric and must not be reported as MISSING.
            both_capped = all(
                (rec.get('si_output') is None
                 and (rec.get('si_output_note') or '').strip() == SI_FACE_CAP_NOTE)
                for rec in sides.values())
            check = tuple(k for k in required
                          if not (both_capped and k == 'si_output'))
            missing = sorted({k for rec in sides.values() for k in check
                              if rec.get(k) is None})
            verdict = item.get('verdict') or 'MISSING'
            reason = item.get('verdict_reason')
            if missing and verdict not in ('WORSE', 'FAIL', 'ERROR', 'TIMEOUT'):
                # A metric the judge needs never arrived; the run cannot be
                # called a pass on the metrics that survived.  TIMEOUT is kept
                # as itself: its metrics are None by construction and it has its
                # own board line.
                verdict = 'MISSING'
                reason = 'missing metric(s): %s' % ', '.join(missing)
            rows.append({'id': item.get('name'), 'verdict': verdict,
                         'reason': reason,
                         'baseline': sides['baseline'],
                         'candidate': sides['candidate'],
                         'strict_watertight': {
                             'baseline': strict_watertight(sides['baseline']),
                             'candidate': strict_watertight(sides['candidate']),
                         } if quality else None,
                         'missing': missing})
    else:
        err = {'rc': r.returncode, 'error_type': 'MissingSummary',
               'tail': (r.stderr.strip().splitlines()[-1][:200]
                        if r.stderr.strip() else '')}
    if err:
        json.dump(err, open('error.json', 'w'), indent=1)
    json.dump({'set': set_name, 'shard': shard, 'shards': shards,
               'mode': mode, 'rc': r.returncode, 'meshes': len(files),
               'rows': rows, 'error': err},
              open('shard-%s-%d.json' % (set_name, shard), 'w'), indent=1)
    sys.exit(0 if not err else 1)


if __name__ == '__main__':
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:
        json.dump({'rc': 1, 'error_type': type(e).__name__, 'tail': str(e)[:200]},
                  open('error.json', 'w'))
        traceback.print_exc()
        sys.exit(1)
