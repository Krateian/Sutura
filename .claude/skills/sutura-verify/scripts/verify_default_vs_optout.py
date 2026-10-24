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
import shutil
import subprocess
import sys
import tempfile
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import verify_repair_shape as vrs  # noqa: E402
try:
    import verify_visual_qa as vvq
except ImportError:
    vvq = None

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


def _sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def _repo_head(repo):
    try:
        return subprocess.check_output(
            ['git', '-C', repo, 'rev-parse', 'HEAD'], text=True,
            stderr=subprocess.DEVNULL).strip()
    except Exception:  # noqa: BLE001
        rp = os.path.join(repo, 'sutura', 'repair.py')
        return _sha256_file(rp) if os.path.exists(rp) else 'unknown'


def _cache_key(in_path, flags, repo, python):
    payload = json.dumps([flags, _repo_head(repo), python], sort_keys=True)
    return hashlib.sha256(
        (_sha256_file(in_path) + '|' + payload).encode()).hexdigest()


def _read_cache(cache_dir, key):
    d = os.path.join(cache_dir, key[:2], key)
    stl, meta = os.path.join(d, 'output.stl'), os.path.join(d, 'meta.json')
    if os.path.exists(stl) and os.path.exists(meta):
        with open(meta) as f:
            return stl, json.load(f)
    return None, None


def _write_cache(cache_dir, key, stl, meta):
    d = os.path.join(cache_dir, key[:2], key)
    os.makedirs(d, exist_ok=True)
    tmp = os.path.join(d, 'output.stl.tmp')
    shutil.copyfile(stl, tmp)
    os.replace(tmp, os.path.join(d, 'output.stl'))
    tmpm = os.path.join(d, 'meta.json.tmp')
    with open(tmpm, 'w') as f:
        json.dump(meta, f)
    os.replace(tmpm, os.path.join(d, 'meta.json'))


def ftetwild_adopted(report):
    ft = (report or {}).get('experimental_ftetwild') or {}
    return bool(ft.get('adopted'))


def judge_quality(b, c, tol):
    """Candidate vs baseline: no mesh worse on the quality metrics."""
    if b.get('si_output') is None or c.get('si_output') is None:
        return 'ERROR', 'SI-after unavailable (hard fail)'
    md5_changed = bool(b.get('md5') and c.get('md5') and b['md5'] != c['md5'])
    if c['reload_holes'] > b['reload_holes']:
        return 'WORSE', 'more holes: %d vs %d' % (c['reload_holes'], b['reload_holes'])
    if c['reload_non_manifold_edges'] > b['reload_non_manifold_edges']:
        return 'WORSE', 'more NM edges: %d vs %d' % (
            c['reload_non_manifold_edges'], b['reload_non_manifold_edges'])
    if c['si_output'] > b['si_output']:
        return 'WORSE', 'SI-after rose: %d vs %d' % (c['si_output'], b['si_output'])
    if c['two_sided_max_mm'] > b['two_sided_max_mm'] + tol:
        return 'WORSE', 'two-sided max worse beyond tol: %s vs %s' % (
            c['two_sided_max_mm'], b['two_sided_max_mm'])
    if c['two_sided_p95_mm'] > b['two_sided_p95_mm'] + tol:
        return 'WORSE', 'two-sided p95 worse beyond tol: %s vs %s' % (
            c['two_sided_p95_mm'], b['two_sided_p95_mm'])
    if c['volume_error_rel'] > b['volume_error_rel'] + 1e-3:
        return 'WORSE', 'volume error rose: %.6f vs %.6f' % (
            c['volume_error_rel'], b['volume_error_rel'])
    bp = b['orca_number_of_parts'] if b['orca_number_of_parts'] is not None else b['components']
    cp = c['orca_number_of_parts'] if c['orca_number_of_parts'] is not None else c['components']
    if cp > bp:
        return 'WORSE', 'more parts: %s vs %s' % (cp, bp)
    if c['closed_shells'] < b['closed_shells']:
        return 'WORSE', 'fewer closed shells: %d vs %d' % (
            c['closed_shells'], b['closed_shells'])
    if (c['reload_holes'] < b['reload_holes']
            or c['reload_non_manifold_edges'] < b['reload_non_manifold_edges']
            or c['si_output'] < b['si_output']
            or c['two_sided_max_mm'] < b['two_sided_max_mm']
            or c['volume_error_rel'] < b['volume_error_rel']
            or cp < bp
            or c['closed_shells'] > b['closed_shells']):
        return 'BETTER', 'no metric worse and at least one improves'
    if md5_changed:
        return 'CHANGED', 'output changed within quality tolerance'
    return 'SAME', 'no quality change'


def _markdown_modes(results, mode):
    if mode == 'byte':
        lines = ['# Byte gate (refactors)', '',
                 '| Mesh | Baseline MD5 | Candidate MD5 | Verdict | Note |',
                 '|---|---|---|---|---|']
        for r in results:
            b = r.get('baseline') or {}
            c = r.get('candidate') or {}
            lines.append('| %s | %s | %s | **%s** | %s |' % (
                r['name'], (b.get('md5') or '')[:8], (c.get('md5') or '')[:8],
                r['verdict'], r.get('verdict_reason', '')))
        return '\n'.join(lines) + '\n'
    lines = ['# Quality gate (behaviour changes)', '',
             '| Mesh | Holes b/c | NM b/c | SI b/c | 2-sided max b/c | '
             'Vol err b/c | Parts b/c | Verdict | Changed |',
             '|---|---|---|---|---|---|---|---|---|']
    for r in results:
        b = r.get('baseline') or {}
        c = r.get('candidate') or {}
        if not b or not c:
            lines.append('| %s | - | - | - | - | - | - | **%s** | - |' % (
                r['name'], r['verdict']))
            continue
        bp = b['orca_number_of_parts'] if b['orca_number_of_parts'] is not None else b['components']
        cp = c['orca_number_of_parts'] if c['orca_number_of_parts'] is not None else c['components']
        lines.append('| %s | %d/%d | %d/%d | %s/%s | %.4f/%.4f | %.6f/%.6f | %s/%s | **%s** | %s |' % (
            r['name'], b['reload_holes'], c['reload_holes'],
            b['reload_non_manifold_edges'], c['reload_non_manifold_edges'],
            b['si_output'], c['si_output'],
            b['two_sided_max_mm'], c['two_sided_max_mm'],
            b['volume_error_rel'], c['volume_error_rel'], bp, cp,
            r['verdict'], 'yes' if r.get('changed') else 'no'))
    vqa_bad = [r for r in results if r.get('verdict') == 'VISUAL-WORSE']
    if vqa_bad:
        lines.append('')
        lines.append('## Visual QA Failures')
        for r in vqa_bad:
            vqa = r.get('vqa') or {}
            sheet = vqa.get('contact_sheet') or ''
            sheet_str = f' (sheet: {sheet})' if sheet else ''
            lines.append('* **%s**: %s%s' % (r['name'], r.get('verdict_reason', ''), sheet_str))
    v2_review = []
    for r in results:
        v2 = ((r.get('vqa') or {}).get('v2') or {})
        flags = v2.get('flags') or []
        sheet = v2.get('contact_sheet')
        if flags or sheet:
            v2_review.append((r['name'], flags, sheet))
    if v2_review:
        lines.append('')
        lines.append('## Visual QA v2 review aids')
        lines.append('')
        lines.append('Optional detailed sheets; the table above and the V1 sheet remain the gate.')
        for name, flags, sheet in v2_review:
            flag_str = ', '.join(flags) if flags else 'forced'
            sheet_str = f' (sheet: {sheet})' if sheet else ''
            lines.append('* **%s**: %s%s' % (name, flag_str, sheet_str))
    return '\n'.join(lines) + '\n'


def main():
    ap = argparse.ArgumentParser(description='baseline/candidate or default-vs-opt-out verification')
    ap.add_argument('meshes', nargs='*', help='STL/OBJ files or directories')
    ap.add_argument('--repo', default=None, help='candidate repository root')
    ap.add_argument('--python', default=sys.executable,
                    help='candidate interpreter with pymeshlab')
    ap.add_argument('--baseline-repo', default=None,
                    help='baseline repository root (byte/quality)')
    ap.add_argument('--baseline-python', default=None,
                    help='baseline interpreter (default: --python)')
    ap.add_argument('--byte', action='store_true', help='refactor: byte-identical outputs')
    ap.add_argument('--quality', action='store_true', help='behaviour: no quality metric worse')
    ap.add_argument('--deterministic', action='store_true',
                    help='add --no-graft --no-fallback-ftetwild --deep-repair off')
    ap.add_argument('--exclude-nondeterministic', action='store_true',
                    help='byte mode: fTetWild-adopting meshes are SKIPPED-NONDET')
    ap.add_argument('--out-dir', default=None)
    ap.add_argument('--cache-dir', default=os.environ.get(
        'SUTURA_VERIFY_CACHE', os.path.expanduser('~/.cache/sutura-verify')),
        help='main/baseline output cache (SHA + flags)')
    ap.add_argument('--jobs', type=int, default=1, help='parallel meshes (e.g. 4)')
    ap.add_argument('--opt-out-flag', default=None)
    ap.add_argument('--opt-out-env', default=None)
    ap.add_argument('--extra-arg', action='append', default=[])
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--timeout', type=int, default=900)
    ap.add_argument('--visual', dest='visual', action='store_true', default=None,
                    help='visual QA: render comparison on changed meshes (default on with --quality)')
    ap.add_argument('--no-visual', dest='visual', action='store_false',
                    help='disable visual QA render comparison')
    args = ap.parse_args()
    mode = 'byte' if args.byte else 'quality' if args.quality else 'feature'
    visual_enabled = args.visual if args.visual is not None else (mode == 'quality')
    has_visual = (vvq is not None and getattr(vvq, 'HAS_PYSIDE6', False))
    if not has_visual:
        if args.visual is True:
            print('error: PySide6 is required for --visual', file=sys.stderr)
            return 2
        elif args.visual is None and mode == 'quality':
            print('warning: PySide6 not installed; visual QA skipped in quality mode', file=sys.stderr)
            visual_enabled = False
        else:
            visual_enabled = False

    if args.exclude_nondeterministic and mode == 'feature':
        print('error: --exclude-nondeterministic needs --byte or --quality', file=sys.stderr)
        return 2
    if mode != 'feature' and not args.baseline_repo:
        print('error: --baseline-repo is required with --byte/--quality', file=sys.stderr)
        return 2
    repo = args.repo or vrs.default_repo()
    baseline_repo = args.baseline_repo or repo
    baseline_python = args.baseline_python or args.python
    repair_py = os.path.join(repo, 'sutura', 'repair.py')
    base_repair_py = os.path.join(baseline_repo, 'sutura', 'repair.py')
    if not os.path.exists(repair_py) or not os.path.exists(base_repair_py):
        print('error: repair.py not found in candidate/baseline repo', file=sys.stderr)
        return 2
    meshes = resolve_meshes(args.meshes, repo, args.limit)
    if not meshes:
        print('error: no meshes', file=sys.stderr)
        return 2
    out_dir = args.out_dir or tempfile.mkdtemp(prefix='sutura-verify-')
    side_names = (('default', 'optout') if mode == 'feature'
                  else ('baseline', 'candidate'))
    for side in side_names:
        os.makedirs(os.path.join(out_dir, side), exist_ok=True)
    if mode != 'feature':
        os.makedirs(args.cache_dir, exist_ok=True)
    print('mode: %s | repo: %s | baseline: %s' % (mode, repo, baseline_repo))
    print('python: %s | baseline python: %s' % (args.python, baseline_python))
    print('meshes: %d | output: %s' % (len(meshes), out_dir))
    env = os.environ.copy()
    env.setdefault('QT_QPA_PLATFORM', 'offscreen')
    det_args = (['--no-graft', '--no-fallback-ftetwild',
                 '--deep-repair', 'off'] if args.deterministic else [])
    common_flags = args.extra_arg + det_args
    opt_flags = ([args.opt_out_flag] if args.opt_out_flag else [])

    def run_side(in_path, name, side, repo_x, python_x, extra, use_cache):
        out_stl = os.path.join(out_dir, side, '%s.stl' % name)
        flags = common_flags + extra
        key = None
        if use_cache:
            key = _cache_key(in_path, flags, repo_x, python_x)
            cached, meta = _read_cache(args.cache_dir, key)
            if cached:
                shutil.copyfile(cached, out_stl)
                meta['time_s'] = 0.0
                meta['cached'] = True
                meta['output'] = out_stl
                print('  %s: cached (%s)' % (side, meta.get('md5', '')[:8]), flush=True)
                return meta
        cmd = [python_x, os.path.join(repo_x, 'sutura', 'repair.py'), in_path,
               '-o', out_stl, '--no-history'] + flags
        run_env = env.copy()
        if side in ('optout', 'candidate') and args.opt_out_env:
            k, v = args.opt_out_env.split('=', 1)
            run_env[k] = v
        rc, sout, serr, el = run_cli(cmd, run_env, repo_x, args.timeout)
        if not os.path.exists(out_stl):
            print('  %s: no output (rc=%d) %s' % (side, rc, serr.strip()[:160]), flush=True)
            return None
        report = cli_report(sout)
        with open(out_stl, 'rb') as f:
            md5 = hashlib.md5(f.read()).hexdigest()
        if mode in ('quality', 'feature'):
            meas = measure(in_path, out_stl, el, repo_x)
        else:
            meas = {'time_s': round(el, 2)}
        meas.update({'md5': md5, 'ftetwild_adopted': ftetwild_adopted(report),
                     'cli_report': {'experimental_ftetwild':
                                    report.get('experimental_ftetwild')}})
        meas.setdefault('cached', False)
        if use_cache:
            _write_cache(args.cache_dir, key, out_stl, meas)
        print('  %s: rc=%d %.2fs holes=%s nm=%s' % (
            side, rc, el, meas.get('reload_holes', '-'), meas.get('reload_non_manifold_edges', '-')),
            flush=True)
        return meas

    def run_one(idx_in):
        idx, in_path = idx_in
        name = os.path.splitext(os.path.basename(in_path))[0]
        print('\n[%d/%d] %s' % (idx, len(meshes), name), flush=True)
        if mode == 'feature':
            b = run_side(in_path, name, 'default', repo, args.python, [], False)
            c = run_side(in_path, name, 'optout', repo, args.python, opt_flags, False)
            key_b, key_c = 'default', 'optout'
        else:
            b = run_side(in_path, name, 'baseline', baseline_repo, baseline_python,
                         [], True)
            c = run_side(in_path, name, 'candidate', repo, args.python, [], False)
            key_b, key_c = 'baseline', 'candidate'
        if b is None or c is None:
            return {'name': name, key_b: b or {}, key_c: c or {},
                    'verdict': 'ERROR', 'verdict_reason': 'a run produced no output',
                    'changed': True}
        nondet = (args.exclude_nondeterministic
                  and (b.get('ftetwild_adopted') or c.get('ftetwild_adopted')))
        if mode == 'feature':
            verdict, reason = judge_verdict(b, c, b.get('tolerance_mm', 0.05))
        elif nondet:
            verdict, reason = 'SKIPPED-NONDET', 'fTetWild adopted on this mesh'
        elif mode == 'byte':
            changed = b['md5'] != c['md5']
            if not changed:
                verdict, reason = 'SAME', 'byte-identical'
            else:
                verdict, reason = 'CHANGED', 'output bytes differ'
        else:
            verdict, reason = judge_quality(b, c, b.get('tolerance_mm', 0.05))
        changed = bool(b.get('md5') != c.get('md5'))

        # Visual QA follows the judge's non-determinism rule: an adopted fTetWild
        # result is stochastic, so the comparison is skipped (in every run,
        # --exclude-nondeterministic or not) and never turns into VISUAL-WORSE.
        mesh_nondet = bool(b.get('ftetwild_adopted') or c.get('ftetwild_adopted'))
        vqa_res = None
        if visual_enabled and vvq is not None and changed and verdict in ('CHANGED', 'BETTER'):
            if mesh_nondet:
                vqa_res = {'skipped': 'nondet'}
            else:
                b_stl = os.path.join(out_dir, key_b, '%s.stl' % name)
                c_stl = os.path.join(out_dir, key_c, '%s.stl' % name)
                if os.path.exists(b_stl) and os.path.exists(c_stl):
                    try:
                        sheet_path = os.path.join(out_dir, '%s_vqa_diff.png' % name)
                        vqa_res = vvq.evaluate_visual_diff(in_path, b_stl, c_stl,
                                                           out_sheet=sheet_path,
                                                           ref_is_input=True)
                        if vqa_res and vqa_res.get('is_visual_worse'):
                            verdict = 'VISUAL-WORSE'
                            reason = vqa_res['reason']
                    except Exception as ex:
                        print('  visual QA warning: %s' % ex, flush=True)

        print('  -> %s (%s)' % (verdict, reason), flush=True)
        res = {'name': name, key_b: b, key_c: c, 'verdict': verdict,
               'verdict_reason': reason, 'changed': changed}
        if vqa_res:
            res['vqa'] = vqa_res
        return res

    if visual_enabled and vvq is not None:
        vvq.ensure_qapp()

    results = []
    jobs = max(1, int(args.jobs))
    if jobs == 1:
        for item in enumerate(meshes, 1):
            results.append(run_one(item))
    else:
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max_workers=jobs) as ex:
            results = list(ex.map(run_one, enumerate(meshes, 1)))

    with open(os.path.join(out_dir, 'summary.json'), 'w') as f:
        json.dump(results, f, indent=2)
    if mode == 'feature':
        md = generate_markdown([r for r in results if r['verdict'] != 'ERROR'],
                               args.opt_out_flag)
    else:
        md = _markdown_modes(results, mode)
    with open(os.path.join(out_dir, 'report.md'), 'w') as f:
        f.write(md)
    bad = [r['name'] for r in results if r['verdict'] in ('WORSE', 'CHANGED', 'ERROR', 'VISUAL-WORSE')]
    skipped = [r['name'] for r in results if r['verdict'] == 'SKIPPED-NONDET']
    print('\nwrote %s/summary.json and report.md' % out_dir)
    print('FAIL: %s' % (', '.join(bad) if bad else 'none'))
    if skipped:
        print('SKIPPED-NONDET: %s' % ', '.join(skipped))
    print('(%s)' % PRIVATE_HINT)
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
