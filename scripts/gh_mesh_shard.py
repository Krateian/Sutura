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
import re
import shutil
import subprocess
import sys
import tempfile
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



# --- Stage fingerprints (opt-in) -------------------------------------------
#
# `SUTURA_STAGE_FP=1` in ENV_CAND makes the Rust pipeline print one line per
# stage on stderr:
#
#     [stage-fp] <label> <geometry-hash> <faces> <verts>
#
# (labels: after_prepass, after_stage1, after_stage2, after_pweld). The hash is a
# 64-bit rolling digest of the stage's geometry; faces/verts are counts. No paths,
# no names, no geometry — safe to publish in a shard summary.
#
# The point: `broken_37111` and `broken_38631` diverge from Python only on Linux,
# and a byte diff alone cannot say which stage introduced it. Comparing these
# hashes stage by stage turns "the outputs differ" into "the outputs first
# differ after stage N".
#
# The harness (`verify_default_vs_optout.py`) does not forward CLI stderr, so this
# re-runs the candidate alone with the flag set and parses its stderr. That costs
# one extra repair per mesh, which is why it is opt-in.
STAGE_FP_RE = re.compile(r'^\[stage-fp\] (\S+) (\d+) (\d+) (\d+)\s*$')
STAGE_FP_ENV = 'SUTURA_STAGE_FP'


def stage_fp_requested(env_c):
    """True when ENV_CAND asks for stage fingerprints."""
    for pair in (env_c or '').split(','):
        k, _, v = pair.partition('=')
        if k.strip() == STAGE_FP_ENV and v.strip() in ('1', 'true', 'yes'):
            return True
    return False


def _stage_fp_runner():
    """How to run the candidate, as (argv_prefix, extra_env).

    Two branches need different runners and the merge brought both:

    * On a Rust-port branch the packaged CLI at ``RUST_BIN`` (default
      ``cand-bin/sutura``) emits the stage lines directly, and ``SUTURA_STAGE_FP``
      is inherited from the environment.
    * On main there is no Rust CLI artifact, so the candidate's own Python
      entry point is used and the flag is set explicitly for that child.

    Preferring the CLI keeps a Rust branch measuring the binary it ships; falling
    back to Python is what makes the feature work on main at all.
    """
    cli = os.environ.get('RUST_BIN', os.path.abspath('cand-bin/sutura'))
    if os.path.exists(cli):
        return ([cli], {})
    py = os.path.abspath('cand-venv/bin/python')
    script = os.path.join(os.path.abspath('cand'), 'sutura', 'repair.py')
    if os.path.exists(py) and os.path.exists(script):
        return ([py, script], {})
    return (None, {})


def collect_stage_fp(mesh_path, extra_env):
    """Run the candidate once and return its stage fingerprints, or None.

    Returns a list of ``{label, hash, faces, verts}`` in emission order. Any
    failure yields None: a missing row must read as "not measured", never as
    "unchanged".
    """
    prefix, _ = _stage_fp_runner()
    if prefix is None:
        return None
    out = tempfile.mkdtemp(prefix='stagefp-')
    try:
        env = dict(os.environ)
        for pair in (extra_env or '').split(','):
            k, _, v = pair.partition('=')
            if k.strip():
                env[k.strip()] = v.strip()
        env[STAGE_FP_ENV] = '1'
        proc = subprocess.run(
            prefix + [mesh_path, '-o', os.path.join(out, 'out.stl'),
                      '--no-history', '--no-cache', '--json'],
            capture_output=True, text=True, env=env, timeout=1800)
        stages = []
        for line in proc.stderr.splitlines():
            m = STAGE_FP_RE.match(line.strip())
            if m:
                stages.append({'label': m.group(1), 'hash': m.group(2),
                               'faces': int(m.group(3)),
                               'verts': int(m.group(4))})
        # Optional finer trace: SUTURA_STAGE1_FP=1 adds one line per Stage-1
        # filter.  Kept verbatim (filter name, counts, hash; no mesh names).
        sub = [l.strip() for l in proc.stderr.splitlines()
               if l.startswith('[stage1-fp] ')]
        if stages and sub:
            stages.append({'label': 'stage1_trace', 'lines': sub})
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
    # determinism compares two independent runs of the SAME ref (the
    # candidate, or the base when no candidate is given), never base vs cand.
    if mode == 'io_parity':
        # p10/core's mode: the Rust CLI is compared against the Python path on
        # the I/O surface. It has its own tool and takes no --extra-arg.
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
        if mode != 'determinism':
            cmd += ['--baseline-env', 'SUTURA_IO_ENGINE=python']

        # Is the Rust bridge installed in the candidate? From p10/core: probe
        # once and require Rust when it is there, so a silent Python fallback
        # cannot be scored as SAME. Tolerate a missing interpreter like main did.
        try:
            probe = subprocess.run(
                [os.path.abspath('cand-venv/bin/python'), '-c',
                 'import sutura_core_py'], capture_output=True, text=True)
            bridge = probe.returncode == 0
        except OSError:
            bridge = False

        if env_c:
            if '_ENGINE=rust' in env_c:
                # From main: an explicit Rust-engine request must really run
                # Rust. A missing bridge is a configuration error, not a pass.
                if not bridge:
                    raise SystemExit(
                        'ENV_CAND asks for a Rust engine but the candidate has '
                        'no sutura_core_py bridge')
                if 'SUTURA_REQUIRE_RUST' not in env_c:
                    env_c += ',SUTURA_REQUIRE_RUST=1'
            cmd += ['--opt-out-env', env_c]
        elif bridge:
            cmd += ['--opt-out-env', 'SUTURA_REQUIRE_RUST=1']

        # Repair CLI arguments applied to both sides (e.g. a forced method). The
        # tokens are restricted to a plain flag/value charset so a dispatch input
        # cannot smuggle shell or path syntax into the harness command line.
        for tok in os.environ.get('EXTRA_ARGS', '').split():
            if not re.fullmatch(r'[A-Za-z0-9_.,=-]+', tok):
                raise SystemExit('bad EXTRA_ARGS token')
            cmd.append('--extra-arg=' + tok)

        if mode == 'determinism':
            # The baseline half is cached by input SHA + flags + repo head, so a
            # warmed cache would compare a run against itself. A fresh cache
            # keeps the two runs independent, which is the point of the check.
            cmd += ['--cache-dir', os.path.join(outdir, 'cache')]
    r = subprocess.run(cmd, capture_output=True, text=True)
    rows = []
    log_lines = []
    err = None
    sp = os.path.join(outdir, 'summary.json')
    # Opt-in stage fingerprints: only when ENV_CAND names the flag, so a
    # normal shard pays nothing.
    want_stage_fp = stage_fp_requested(env_c)
    # Summary rows carry the mesh stem, not the manifest path; map it back so
    # the fingerprint run opens the real file instead of a missing one.
    by_stem = {os.path.splitext(os.path.basename(f))[0]: f for f in files}
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
            # Opt-in: only when ENV_CAND names the flag, so a normal shard
            # pays nothing for the extra repair.
            if want_stage_fp:
                fp = collect_stage_fp(resolve_mesh(corpus, by_stem.get(item.get('name'), item.get('name'))),
                                      env_c)
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
