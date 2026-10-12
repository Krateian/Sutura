#!/usr/bin/env python3
"""verify_default_vs_optout.py - shape comparison of a default tier vs its opt-out.

Runs one repair command twice per input mesh - once with the feature ON
(default) and once with it OFF (``--no-tamponade`` by default; any flag can be
given) - and judges the two OUTPUTS by shape, never by the tool's own claims:

  * reload holes / non-manifold edges (``defects.detect`` on the float32 weld),
  * vertex-connected components and fully closed shells,
  * OrcaSlicer ``number_of_parts`` / ``manifold`` (headless ``--info``),
  * two-sided vertex-to-surface distance to the input (max / p95),
  * tolerance ``clamp(bbox_diag / 10000, 0.0125, 0.05)`` mm,
  * outside-the-input leak (added vertices with winding < 0.5 beyond tol),
  * wall-clock runtime of each run.

It is the comparison half of the sutura-verify skill and mirrors the per-mesh
metric logic of the private A18 harness, with no hard-coded paths: the repo is
found by walking up to ``sutura/repair.py`` and the mesh list is an argument.

The verdict is deliberately conservative: a run is worse only when it has more
holes / non-manifold edges / parts, loses manifold status, introduces a leak,
or exceeds the other run's two-sided distance by more than the tolerance.

Usage:
  verify_default_vs_optout.py [MESH ...] [--repo DIR] [--python PY]
        [--out-dir DIR] [--opt-out-flag FLAG] [--limit N] [--timeout S]
        [--no-history]

With no MESH it uses the public sample corpus under
``tests/real-world-samples/*.stl``.  Private samples must be passed by path on
the command line only and are never named in this file; keep their output
under a scratch directory.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import verify_repair_shape as vrs  # noqa: E402

PRIVATE_HINT = ("private samples: pass the path on the command line only; "
                "never add their name to a tracked file")


def resolve_meshes(paths, repo, limit):
    """Expand files/directories into an ordered list of STL paths."""
    out = []
    if paths:
        for p in paths:
            if os.path.isdir(p):
                out.extend(sorted(os.path.join(p, n) for n in os.listdir(p)
                                  if n.lower().endswith(('.stl', '.obj', '.3mf'))))
            elif os.path.exists(p):
                out.append(p)
            else:
                print('warning: no such mesh: %s' % p, file=sys.stderr)
    else:
        corpus = os.path.join(repo, 'tests', 'real-world-samples')
        if os.path.isdir(corpus):
            out = sorted(os.path.join(corpus, n) for n in os.listdir(corpus)
                         if n.lower().endswith('.stl'))
    if limit and limit > 0:
        out = out[:limit]
    return out


def run_cli(cmd, env, cwd, timeout):
    t0 = time.time()
    try:
        p = subprocess.run(cmd, env=env, cwd=cwd, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr, time.time() - t0
    except subprocess.TimeoutExpired:
        return 124, '', 'TIMEOUT after %ss' % timeout, time.time() - t0


def cli_report(stdout):
    """Last JSON object on stdout, or {} when the run failed."""
    for line in reversed((stdout or '').strip().splitlines()):
        line = line.strip()
        if line.startswith('{'):
            try:
                return json.loads(line)
            except Exception:  # noqa: BLE001
                return {}
    return {}


def measure(in_path, out_path, elapsed, repo):
    m = vrs.verify(in_path, out_path, repo=repo)
    m['time_s'] = round(elapsed, 2)
    return m


def judge_verdict(d, n, tol):
    """Return (verdict, reason): d = default, n = opt-out metrics."""
    if d.get('md5') and d.get('md5') == n.get('md5'):
        return 'SAME', 'outputs are byte-identical'
    if d['reload_holes'] > n['reload_holes']:
        return 'WORSE', 'more holes: %d vs %d' % (d['reload_holes'], n['reload_holes'])
    if d['reload_non_manifold_edges'] > n['reload_non_manifold_edges']:
        return 'WORSE', 'more NM edges: %d vs %d' % (
            d['reload_non_manifold_edges'], n['reload_non_manifold_edges'])
    p_d = d['orca_number_of_parts'] if d['orca_number_of_parts'] is not None else d['components']
    p_n = n['orca_number_of_parts'] if n['orca_number_of_parts'] is not None else n['components']
    if p_d > p_n:
        return 'WORSE', 'more parts: %s vs %s' % (p_d, p_n)
    if d['orca_manifold'] == 'no' and n['orca_manifold'] == 'yes':
        return 'WORSE', 'lost manifold status (yes -> no)'
    if d['leak_count'] > 0 and n['leak_count'] == 0:
        return 'WORSE', 'new leak: %s mm (%d pts)' % (d['leak_max_mm'], d['leak_count'])
    if d['leak_max_mm'] > n['leak_max_mm'] + tol:
        return 'WORSE', 'worse leak: %s vs %s mm' % (d['leak_max_mm'], n['leak_max_mm'])
    if d['two_sided_max_mm'] > n['two_sided_max_mm'] + tol:
        return 'WORSE', 'distance max worse beyond tol: %s vs %s (tol=%.4f)' % (
            d['two_sided_max_mm'], n['two_sided_max_mm'], tol)
    if d['two_sided_p95_mm'] > n['two_sided_p95_mm'] + tol:
        return 'WORSE', 'distance p95 worse beyond tol: %s vs %s (tol=%.4f)' % (
            d['two_sided_p95_mm'], n['two_sided_p95_mm'], tol)

    if d['reload_holes'] < n['reload_holes']:
        return 'BETTER', 'fewer holes: %d vs %d' % (d['reload_holes'], n['reload_holes'])
    if d['reload_non_manifold_edges'] < n['reload_non_manifold_edges']:
        return 'BETTER', 'fewer NM edges: %d vs %d' % (
            d['reload_non_manifold_edges'], n['reload_non_manifold_edges'])
    if p_d < p_n:
        return 'BETTER', 'fewer parts: %s vs %s' % (p_d, p_n)
    if d['closed_shells'] > n['closed_shells']:
        return 'BETTER', 'more closed shells: %d vs %d' % (
            d['closed_shells'], n['closed_shells'])
    if d['orca_manifold'] == 'yes' and n['orca_manifold'] == 'no':
        return 'BETTER', 'gained manifold status (no -> yes)'
    if n['leak_count'] > 0 and d['leak_count'] == 0:
        return 'BETTER', 'eliminated leak: %s mm -> 0' % n['leak_max_mm']
    if n['leak_max_mm'] > d['leak_max_mm'] + tol:
        return 'BETTER', 'reduced leak: %s -> %s mm' % (n['leak_max_mm'], d['leak_max_mm'])
    if n['two_sided_max_mm'] > d['two_sided_max_mm'] + tol:
        return 'BETTER', 'distance max better: %s vs %s' % (
            d['two_sided_max_mm'], n['two_sided_max_mm'])
    if n['two_sided_p95_mm'] > d['two_sided_p95_mm'] + tol:
        return 'BETTER', 'distance p95 better: %s vs %s' % (
            d['two_sided_p95_mm'], n['two_sided_p95_mm'])
    return 'SAME', 'shape metrics within tolerance'


def generate_markdown(results, opt_out_flag):
    td = sum(r['default']['time_s'] for r in results)
    tn = sum(r['optout']['time_s'] for r in results)
    verdicts = [r['verdict'] for r in results]
    c_b = verdicts.count('BETTER')
    c_s = verdicts.count('SAME')
    c_w = verdicts.count('WORSE')
    if c_w == 0 and c_b > 0:
        overall = 'BETTER (%d better, %d same, 0 worse across %d)' % (c_b, c_s, len(results))
    elif c_w == 0:
        overall = 'SAME (%d same, 0 worse across %d)' % (c_s, len(results))
    else:
        overall = 'MIXED (%d better, %d same, %d worse across %d)' % (
            c_b, c_s, c_w, len(results))
    lines = [
        '# Default vs `%s`: shape verification' % opt_out_flag,
        '',
        '**Overall verdict:** **%s**  ' % overall,
        '**Runtime:** default **%.2fs** vs opt-out **%.2fs** (delta **%+.2fs**)' % (
            td, tn, td - tn),
        '',
        '| Mesh | Mode | Holes | NM | Comp/Shells | Orca parts/manifold | '
        'Two-sided max/p95 (mm) | Tol (mm) | Leak | Time (s) | Verdict |',
        '|---|---|---|---|---|---|---|---|---|---|---|',
    ]
    for r in results:
        d, n, v = r['default'], r['optout'], r['verdict']
        for tag, m in ((opt_out_flag, n), ('DEFAULT', d)):
            leak = ('%.4f mm (%d pts)' % (m['leak_max_mm'], m['leak_count'])
                    if m['leak_count'] else '0')
            orca = '%s/%s' % (m['orca_number_of_parts']
                              if m['orca_number_of_parts'] is not None else '?',
                              m['orca_manifold'])
            lines.append('| %s | %s | %d | %d | %d/%d | %s | %.4f / %.4f | %.4f | %s | %.2fs | %s |' % (
                r['name'], tag, m['reload_holes'], m['reload_non_manifold_edges'],
                m['components'], m['closed_shells'], orca,
                m['two_sided_max_mm'], m['two_sided_p95_mm'], m['tolerance_mm'],
                leak, m['time_s'], '**%s**' % v if tag == 'DEFAULT' else '-'))
    lines += ['', '## Per-mesh detail', '']
    for r in results:
        lines.append('- `%s` — **%s**: %s' % (r['name'], r['verdict'], r['verdict_reason']))
    return '\n'.join(lines) + '\n'


def main():
    ap = argparse.ArgumentParser(description='default vs opt-out shape comparison')
    ap.add_argument('meshes', nargs='*', help='STL/OBJ files or directories')
    ap.add_argument('--repo', default=None, help='repository root')
    ap.add_argument('--python', default=sys.executable,
                    help='interpreter with pymeshlab (the repair venv)')
    ap.add_argument('--out-dir', default=None,
                    help='output dir (default: a fresh temp dir)')
    ap.add_argument('--opt-out-flag', default='--no-tamponade',
                    help='flag that disables the feature (default: --no-tamponade)')
    ap.add_argument('--limit', type=int, default=0, help='max meshes (0 = all)')
    ap.add_argument('--timeout', type=int, default=900, help='per-run timeout (s)')
    args = ap.parse_args()

    repo = args.repo or vrs.default_repo()
    repair_py = os.path.join(repo, 'sutura', 'repair.py')
    if not os.path.exists(repair_py):
        print('error: %s not found; pass --repo' % repair_py, file=sys.stderr)
        return 2
    meshes = resolve_meshes(args.meshes, repo, args.limit)
    if not meshes:
        print('error: no meshes; pass paths or --repo with a sample corpus',
              file=sys.stderr)
        return 2
    out_dir = args.out_dir or tempfile.mkdtemp(prefix='sutura-verify-')
    os.makedirs(os.path.join(out_dir, 'default'), exist_ok=True)
    os.makedirs(os.path.join(out_dir, 'optout'), exist_ok=True)
    print('repo: %s' % repo)
    print('python: %s' % args.python)
    print('meshes: %d | output: %s' % (len(meshes), out_dir))

    env = os.environ.copy()
    env.setdefault('QT_QPA_PLATFORM', 'offscreen')
    results = []
    for idx, in_path in enumerate(meshes, 1):
        name = os.path.splitext(os.path.basename(in_path))[0]
        print('\n[%d/%d] %s' % (idx, len(meshes), name), flush=True)
        runs = {}
        for key, extra in (('default', []), ('optout', [args.opt_out_flag])):
            out_stl = os.path.join(out_dir, key, '%s.stl' % name)
            cmd = [args.python, repair_py, in_path, '-o', out_stl, '--no-history'] + extra
            rc, sout, serr, el = run_cli(cmd, env, repo, args.timeout)
            meas = measure(in_path, out_stl, el, repo) if os.path.exists(out_stl) else None
            if meas is None:
                print('  %s: no output (rc=%d) %s' % (key, rc, serr.strip()[:160]),
                      file=sys.stderr)
                runs[key] = None
                continue
            with open(out_stl, 'rb') as fh:
                meas['md5'] = hashlib.md5(fh.read()).hexdigest()
            meas['tamponade'] = cli_report(sout).get('tamponade', {})
            runs[key] = meas
            print('  %s: rc=%d %.2fs holes=%d nm=%d' % (
                key, rc, el, meas['reload_holes'], meas['reload_non_manifold_edges']))
        if runs.get('default') is None or runs.get('optout') is None:
            results.append({'name': name, 'verdict': 'ERROR',
                            'verdict_reason': 'a run produced no output',
                            'default': runs.get('default') or {},
                            'optout': runs.get('optout') or {}})
            continue
        d, n = runs['default'], runs['optout']
        verdict, reason = judge_verdict(d, n, d['tolerance_mm'])
        print('  -> %s (%s)' % (verdict, reason))
        results.append({'name': name, 'verdict': verdict, 'verdict_reason': reason,
                        'default': d, 'optout': n})

    with open(os.path.join(out_dir, 'summary.json'), 'w') as f:
        json.dump(results, f, indent=2)
    md = generate_markdown([r for r in results if r['verdict'] != 'ERROR'],
                           args.opt_out_flag)
    with open(os.path.join(out_dir, 'report.md'), 'w') as f:
        f.write(md)
    worse = [r['name'] for r in results if r['verdict'] == 'WORSE']
    print('\nwrote %s/summary.json and report.md' % out_dir)
    print('WORSE: %s' % (', '.join(worse) if worse else 'none'))
    print('(%s)' % PRIVATE_HINT)
    return 1 if worse else 0


if __name__ == '__main__':
    sys.exit(main())
