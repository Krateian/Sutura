#!/usr/bin/env python3
"""One shard: run the verify harness with per-side repos/pythons and real keys.

Each row carries the baseline and the candidate metric record, so the board
can show the visible two-sided values next to the ``*_all_*`` values and the
SI-after pair that decides TRADE-SI.

A missing OrcaSlicer is not a missing row: the harness reports
``orca_number_of_parts=None`` / ``orca_manifold='unknown'`` and the judge falls
back to the component count, so Orca is reported but never required.

``MODE=rust_ab`` is the post-P10 gate: both sides are release CLIs, the
baseline being the previous Rust commit's binary and the candidate this
commit's.  No Python is involved; a row is SAME only when both exit 0 and write
identical bytes.  ``RUST_BASE_BIN`` / ``RUST_BIN`` name the two binaries and
``RUST_AB_FLAGS`` (default ``--json --no-history --no-cache``) the shared args.
"""
import glob
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import traceback

VERIFY = os.environ.get(
    'VERIFY_SCRIPT',
    'tools/.claude/skills/sutura-verify/scripts/verify_default_vs_optout.py')
IO_PARITY = os.environ.get('IO_PARITY', 'tools/scripts/io_parity.py')
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
FLAGS = {'io_parity': [],
         'byte': ['--byte', '--deterministic'],
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



STAGE_FP_RE = re.compile(r'^\[stage-fp\] (\S+) (\d+) (\d+) (\d+)\s*$')


def stage_fingerprints(mesh_path):
    """Run one mesh through the candidate CLI and return its stage fingerprints.

    Only hashes and counts are returned: the `[stage-fp] <label> <hash> <faces>
    <verts>` lines carry no geometry and no file names, so they are safe to put
    in a shard summary. Returns None when the fingerprinting path is unavailable,
    so a missing row means "not requested", never "changed".

    Opt-in via SUTURA_STAGE_FP=1: this runs one extra repair per mesh, which is
    not something every shard should pay for.
    """
    if os.environ.get('SUTURA_STAGE_FP', '') not in ('1', 'true', 'yes'):
        return None
    cli = os.environ.get('RUST_BIN', os.path.abspath('cand-bin/sutura'))
    if not os.path.exists(cli):
        return None
    out = tempfile.mkdtemp(prefix='stagefp-')
    try:
        env = dict(os.environ)
        env['SUTURA_STAGE_FP'] = '1'
        proc = subprocess.run(
            [cli, mesh_path, '-o', os.path.join(out, 'out.stl'), '--json'],
            capture_output=True, text=True, env=env, timeout=1800)
        stages = []
        for line in proc.stderr.splitlines():
            m = STAGE_FP_RE.match(line.strip())
            if m:
                stages.append({'label': m.group(1), 'hash': m.group(2),
                               'faces': int(m.group(3)),
                               'verts': int(m.group(4))})
        return stages or None
    except Exception:
        return None
    finally:
        shutil.rmtree(out, ignore_errors=True)


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


def numeric_log_lines(items):
    """Numeric-only per-side trace: ``row side rc elapsed_ms``.

    Row index is 1-based and maps to ``rows[]`` in the shard JSON; the model
    name stays out of the log. Missing values are written as -1.
    """
    lines = []
    for row_idx, item in enumerate(items, 1):
        for side_idx, side_name in enumerate(('baseline', 'candidate')):
            rec = item.get(side_name) or {}
            rc = rec.get('rc')
            secs = rec.get('time_s')
            lines.append('%d %d %s %s' % (
                row_idx, side_idx,
                rc if rc is not None else -1,
                int(round(float(secs) * 1000)) if secs is not None else -1))
    return lines


RUST_AB_DEFAULT_FLAGS = '--json --no-history --no-cache'


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def run_rust_ab(files, corpus):
    """Byte comparison of two release CLIs (P10's Rust-vs-Rust mode)."""
    baseline = os.path.abspath(os.environ.get('RUST_BASE_BIN', 'base-bin/sutura'))
    candidate = os.path.abspath(os.environ.get('RUST_BIN', 'cand-bin/sutura'))
    for label, path in (('baseline', baseline), ('candidate', candidate)):
        if not os.access(path, os.X_OK):
            raise SystemExit('rust_ab: no executable %s CLI at %s'
                             % (label, path))
    flags = shlex.split(os.environ.get('RUST_AB_FLAGS', RUST_AB_DEFAULT_FLAGS))
    timeout = float(os.environ.get('RUST_AB_TIMEOUT', '1800'))
    rows = []
    log_lines = []
    for idx, rel in enumerate(files, 1):
        src = resolve_mesh(corpus, rel)
        sides = {}
        for label, binary in (('baseline', baseline), ('candidate', candidate)):
            work = tempfile.mkdtemp(prefix='rustab-')
            out = os.path.join(work, 'out.stl')
            env = dict(os.environ)
            env['SUTURA_DATA_DIR'] = os.path.join(work, 'data')
            env['SUTURA_NO_CACHE'] = '1'
            started = time.time()
            try:
                proc = subprocess.run([binary, src, '-o', out] + flags,
                                      capture_output=True, text=True,
                                      timeout=timeout, env=env)
                rc, err = proc.returncode, proc.stderr[-300:]
            except subprocess.TimeoutExpired:
                rc, err = 'TIMEOUT', ''
            elapsed = time.time() - started
            sha = sha256_file(out) if rc == 0 and os.path.exists(out) else None
            sides[label] = {'rc': rc, 'sha256': sha,
                            'seconds': round(elapsed, 3),
                            'error': None if rc == 0 else err}
            log_lines.append('%d %s %s %s' % (
                idx, label, rc, int(round(elapsed * 1000))))
            shutil.rmtree(work, ignore_errors=True)
        base, cand = sides['baseline'], sides['candidate']
        if base['rc'] != 0 or cand['rc'] != 0:
            verdict = 'TIMEOUT' if 'TIMEOUT' in (base['rc'], cand['rc']) else 'RC'
        elif base['sha256'] == cand['sha256']:
            verdict = 'SAME'
        else:
            verdict = 'DIFF'
        rows.append({'id': os.path.basename(rel), 'verdict': verdict,
                     'reason': None if verdict == 'SAME' else
                     'baseline rc=%s candidate rc=%s' % (base['rc'], cand['rc']),
                     'baseline': base, 'candidate': cand})
    return rows, log_lines


def main():
    set_name = os.environ['SET_NAME']
    shard = int(os.environ['SHARD'])
    shards = int(os.environ['SHARDS'])
    mode = os.environ.get('MODE', 'rust_ab')
    env_c = os.environ.get('ENV_CAND', '')
    env_b = os.environ.get('ENV_BASE', '')
    if env_b:
        raise SystemExit('ENV_BASE is not supported by the harness '
                         '(candidate-side env only)')
    quality = mode in QUALITY_MODES
    if mode == 'io_parity':
        keys = ('format', 'meshes', 'verts', 'tris', 'array_sha256',
                'write_sha256', 'unit_warnings', 'material_refs', 'scan')
        required = ('format', 'meshes', 'verts', 'tris', 'array_sha256',
                    'write_sha256')
    else:
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
    # P10: after the Python runtime is gone this is the only shape gate that
    # runs; both sides are release binaries, so nothing here imports sutura.
    if mode == 'rust_ab':
        rows, log_lines = run_rust_ab(files, corpus)
        json.dump({'set': set_name, 'shard': shard, 'shards': shards,
                   'mode': mode, 'rc': 0, 'meshes': len(files),
                   'rows': rows, 'error': None},
                  open('shard-%s-%d.json' % (set_name, shard), 'w'), indent=1)
        with open('shard-%s-%d.log' % (set_name, shard), 'w') as handle:
            if log_lines:
                handle.write('\n'.join(log_lines) + '\n')
        failed = sum(1 for row in rows if row['verdict'] != 'SAME')
        sys.exit(1 if failed else 0)
    # determinism compares two independent runs of the SAME ref (the
    # candidate, or the base when no candidate is given), never base vs cand.
    if mode == 'io_parity':
        rust_bin = os.environ.get('RUST_BIN',
                                  os.path.abspath('cand-bin/sutura'))
        cmd = [sys.executable, IO_PARITY] \
            + [resolve_mesh(corpus, f) for f in files] + [
                '--baseline-repo', os.path.abspath('base'),
                '--baseline-python', os.path.abspath('base-venv/bin/python'),
                '--rust-bin', rust_bin,
                '--out-dir', outdir]
    else:
        a_repo, a_py = (('cand', 'cand-venv') if mode == 'determinism'
                        else ('base', 'base-venv'))
        cmd = [sys.executable, VERIFY] \
            + [resolve_mesh(corpus, f) for f in files] + FLAGS[mode] + [
                '--baseline-repo', os.path.abspath(a_repo),
                '--baseline-python', os.path.abspath(a_py + '/bin/python'),
                '--repo', os.path.abspath('cand'),
                '--python', os.path.abspath('cand-venv/bin/python'),
                '--out-dir', outdir]
        if mode not in ('determinism', 'io_parity'):
            # P10: the product default is the Rust backend; the comparison's
            # baseline side must still be the legacy Python pipeline.
            cmd += ['--baseline-env', 'SUTURA_BACKEND=python']
            cmd += ['--baseline-env', 'SUTURA_IO_ENGINE=python']
            try:
                probe = subprocess.run(
                    [os.path.abspath('cand-venv/bin/python'), '-c',
                     'import sutura_core_py'],
                    capture_output=True, text=True)
                bridge = probe.returncode == 0
            except OSError:
                bridge = False
        else:
            bridge = False
        if env_c:
            cmd += ['--opt-out-env', env_c]
        if bridge:
            cmd += ['--opt-out-env', 'SUTURA_REQUIRE_RUST=1']
        if mode == 'determinism':
            # The baseline half is cached by input SHA + flags + repo head, so a
            # warmed cache would compare a run against itself.  A fresh cache
            # keeps the two runs independent, which is the point of the check.
            cmd += ['--cache-dir', os.path.join(outdir, 'cache')]
    r = subprocess.run(cmd, capture_output=True, text=True)
    rows = []
    log_lines = []
    err = None
    sp = os.path.join(outdir, 'summary.json')
    if os.path.exists(sp):
        summary_items = json.load(open(sp))
        log_lines = numeric_log_lines(summary_items)
        for item in summary_items:
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
            if missing and verdict not in ('WORSE', 'FAIL', 'ERROR', 'TIMEOUT',
                                           'CRASHED'):
                # A metric the judge needs never arrived; the run cannot be
                # called a pass on the metrics that survived.  TIMEOUT/CRASHED
                # are kept as themselves: their metrics are None by construction
                # and they have their own board lines.
                verdict = 'MISSING'
                reason = 'missing metric(s): %s' % ', '.join(missing)
            row = {'id': item.get('name'), 'verdict': verdict,
                         'reason': reason,
                         'baseline': sides['baseline'],
                         'candidate': sides['candidate'],
                         'rc': {side: (item.get(side) or {}).get('rc')
                                for side in ('baseline', 'candidate')},
                         'seconds': {side: (item.get(side) or {}).get('time_s')
                                     for side in ('baseline', 'candidate')},
                         'strict_watertight': {
                             'baseline': strict_watertight(sides['baseline']),
                             'candidate': strict_watertight(sides['candidate']),
                         } if quality else None,
                         'missing': missing}
            # Stage-by-stage fingerprints, when requested, so a byte difference
            # can be localised to the stage that produced it.
            fp = stage_fingerprints(resolve_mesh(corpus, item.get('name')))
            if fp:
                row['stage_fp'] = fp
            rows.append(row)
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
    # Numeric-only trace (row side rc elapsed_ms). Names stay in the JSON.
    with open('shard-%s-%d.log' % (set_name, shard), 'w') as f:
        if log_lines:
            f.write('\n'.join(log_lines) + '\n')
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
