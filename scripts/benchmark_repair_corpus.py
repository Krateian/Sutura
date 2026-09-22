"""Repair-corpus benchmark with a STRICT watertight metric.

Runs the current repair pipeline (auto mode) over every STL in a corpus
directory and measures the FINAL output geometry (the exact arrays that are
saved to the `_fixed` file) with a strict defects.detect() closed-loop check:

    strict_watertight == (holes == 0 and non_manifold == 0)

The pipeline's own verdict (`repair_mesh_from_arrays` + stage 2) is recorded
alongside so a claim-vs-check discrepancy is visible, and input defects are
captured with the same detector for an input-vs-output comparison. Input and
output self-intersection counts are measured too (`input_self_intersections`
/ `output_self_intersections`, pymeshlab's per-face selection) so SI-focused
experiments (e.g. the autorefine prototype) can be tracked before/after; the
strict watertight metric itself deliberately excludes SI.

This is a measurement/benchmark harness, not a pass-fail test. It must run
under the PyMeshLab venv (it imports repair.py):

    ~/.local/share/sutura/venv/bin/python scripts/benchmark_repair_corpus.py \
        /path/to/corpus /path/to/out.json

Defaults: corpus = /tmp/sutura_corpus_100, out = /tmp/sutura_repair_benchmark.json
"""
import gc
import json
import os
import sys
import tempfile
import time

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import classification  # noqa: E402
import defects  # noqa: E402

import repair  # noqa: E402
import manifold_bridge  # noqa: E402  (optional manifold3d cross-validation)


def load_input(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    return v, t


def count_self_intersections(v, t):
    """Count self-intersecting faces of a mesh (pymeshlab's per-face SI
    selection). Mirrors repair.dry_run_mesh_from_arrays' measurement so the
    harness sees the same number the pipeline's validate/dry-run report."""
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float32),
                        face_matrix=np.asarray(t, np.int32)))
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    return int(ms.current_mesh().face_selection_array().sum())


def run_one(src, tmpdir, autorefine=False, ftetwild=False):
    """Repair one mesh and return (entry, error)."""
    entry = {'file': os.path.basename(src),
             'size_bytes': os.path.getsize(src)}
    t0 = time.time()
    bad = repair.scan_bad_coordinates(src)
    if bad:
        return None, {'file': os.path.basename(src),
                      'error': 'bad_coordinates: %s' % bad, 'dt': time.time() - t0}
    try:
        v, t = load_input(src)
        entry['input_verts'] = int(len(v))
        entry['input_faces'] = int(len(t))
        in_det = defects.detect(v, t)
        entry['input_holes'] = len(in_det['holes'])
        entry['input_non_manifold'] = len(in_det['non_manifold'])
        entry['input_self_intersections'] = count_self_intersections(v, t)

        rep, new_v, new_t = repair.repair_mesh_from_arrays(
            v, t, tmpdir, mode='auto', autorefine=autorefine,
            ftetwild=ftetwild)
        new_v, new_t = repair.maybe_run_stage2(rep, new_v, new_t, tmpdir)

        category, _issues, _summary = classification.classify(rep)
        entry['category'] = category
        s1 = rep.get('stage1', {})
        entry['stage1_two_manifold'] = bool(s1.get('two_manifold'))
        entry['stage1_holes_remaining'] = s1.get('holes_remaining')
        entry['stage1_non_manifold_edges_remaining'] = s1.get('non_manifold_edges_remaining')
        entry['detected_type'] = rep.get('detected_type')
        entry['detected_confidence'] = rep.get('detected_confidence')
        entry['repair_mode'] = rep.get('repair_mode')
        s2 = rep.get('stage2')
        if s2 is not None:
            entry['stage2_ok'] = bool('error' not in s2)
            entry['stage2_error'] = s2.get('error')
        else:
            entry['stage2_ok'] = None

        out_det = defects.detect(new_v, new_t)
        entry['strict_holes'] = len(out_det['holes'])
        entry['strict_non_manifold'] = len(out_det['non_manifold'])
        entry['strict_watertight'] = bool(
            entry['strict_holes'] == 0 and entry['strict_non_manifold'] == 0)
        # Self-intersections measured on the FINAL output arrays (the stage-1
        # pipeline value is reported separately as stage1_si_remaining, when
        # the report carries it). Kept distinct from the strict holes/nm
        # metric on purpose: SI stays a separate signal (see the benchmark
        # doc), but the harness now tracks it so SI-focused experiments (e.g.
        # autorefine) can be measured before/after.
        entry['output_self_intersections'] = count_self_intersections(new_v, new_t)
        entry['stage1_si_remaining'] = s1.get('self_intersections_remaining')
        # fTetWild fallback tier (FAZ17) outcome when enabled: whether it ran,
        # was adopted, and whether a manifold3d post-process was applied.
        ft = rep.get('experimental_ftetwild')
        if isinstance(ft, dict):
            entry['ftetwild_ran'] = bool(ft.get('ran'))
            entry['ftetwild_adopted'] = bool(ft.get('adopted'))
            entry['ftetwild_manifold_postprocessed'] = bool(
                ft.get('manifold_postprocessed'))
            entry['ftetwild_time'] = ft.get('time')
            entry['ftetwild_error'] = ft.get('error')
        else:
            entry['ftetwild_ran'] = False
        # Optional manifold3d cross-validation (independent verdict; None when
        # manifold3d is unavailable -> column reported as n/a).
        m3_ok, m3_status = manifold_bridge.watertight_check(new_v, new_t)
        entry['manifold3d_watertight'] = m3_ok
        entry['manifold3d_status'] = m3_status
        entry['m3d_consistent'] = (
            None if m3_ok is None
            else bool(m3_ok == entry['strict_watertight']))
        entry['output_verts'] = int(len(new_v))
        entry['output_faces'] = int(len(new_t))
        return entry, None
    except Exception as e:  # noqa: BLE001 - robustness: one bad file never kills the run
        return None, {'file': os.path.basename(src),
                      'error': '%s: %s' % (type(e).__name__, e),
                      'dt': time.time() - t0}
    finally:
        entry['dt'] = round(time.time() - t0, 2)


def main():
    import argparse
    p = argparse.ArgumentParser(
        description='Repair-corpus benchmark with a strict watertight metric.')
    p.add_argument('corpus', nargs='?', default='/tmp/sutura_corpus_100',
                   help='corpus dir (default: /tmp/sutura_corpus_100)')
    p.add_argument('out', nargs='?', default='/tmp/sutura_repair_benchmark.json',
                   help='output JSON path')
    p.add_argument('--experimental-autorefine', action='store_true',
                   help='enable --experimental-autorefine for every mesh')
    p.add_argument('--experimental-fallback-ftetwild', action='store_true',
                   help='enable --experimental-fallback-ftetwild for every mesh')
    args = p.parse_args()
    corpus, out_path = args.corpus, args.out
    files = sorted(f for f in os.listdir(corpus) if f.lower().endswith('.stl'))
    flags = (' +autorefine' if args.experimental_autorefine else '') + \
            (' +ftetwild' if args.experimental_fallback_ftetwild else '')
    print('corpus: %s (%d STL files)%s' % (corpus, len(files), flags), flush=True)

    results = {}
    failures = []
    for i, f in enumerate(files, 1):
        tmp = tempfile.mkdtemp(prefix='sutura-bench-')
        entry = err = None
        try:
            entry, err = run_one(os.path.join(corpus, f), tmp,
                                 autorefine=args.experimental_autorefine,
                                 ftetwild=args.experimental_fallback_ftetwild)
        except Exception as e:  # noqa: BLE001 - never let one file kill the run
            err = {'file': f, 'error': '%s: %s' % (type(e).__name__, e)}
        finally:
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)
        if err is not None:
            failures.append(err)
            results[f] = err
            print('[%3d/%d] %-48s ERROR %s' % (i, len(files), f[:48], err['error'][:60]), flush=True)
        else:
            results[f] = entry
            m3 = entry.get('manifold3d_watertight')
            m3s = 'WT' if m3 is True else ('OPEN' if m3 is False else 'n/a')
            fta = ' adv' if entry.get('ftetwild_adopted') else (' run' if entry.get('ftetwild_ran') else '')
            print('[%3d/%d] %-48s cat=%-10s strict=%s m3d=%s (holes=%d nm=%d si=%d->%d)%s' % (
                i, len(files), f[:48], str(entry.get('category')),
                entry['strict_watertight'], m3s, entry['strict_holes'],
                entry['strict_non_manifold'],
                entry.get('input_self_intersections', 0),
                entry.get('output_self_intersections', 0), fta), flush=True)
        del entry, err
        gc.collect()

    with open(out_path, 'w') as fh:
        json.dump(results, fh, indent=1)

    n = len(results)
    strict_wt = sum(1 for r in results.values() if r.get('strict_watertight'))
    cat_watertight = sum(1 for r in results.values() if r.get('category') == 'watertight')
    cat_warning = sum(1 for r in results.values() if r.get('category') == 'warning')
    cat_error = sum(1 for r in results.values() if r.get('category') == 'error')
    # claim vs strict-check cross-tab
    both = [(f, r) for f, r in results.items()
            if r.get('category') == 'watertight' and not r.get('strict_watertight')]
    claim_gap = [(f, r) for f, r in results.items()
                 if r.get('category') != 'watertight' and r.get('strict_watertight')]
    print()
    print('=== strict watertight benchmark ===')
    print('meshes                : %d' % n)
    print('strict watertight     : %d (%.1f%%)' % (strict_wt, 100.0 * strict_wt / n))
    print('pipeline watertight   : %d' % cat_watertight)
    print('pipeline warning      : %d' % cat_warning)
    print('pipeline error        : %d' % cat_error)
    print('errors                : %d' % len(failures))
    print('watertight claim but strict NOT watertight: %d' % len(both))
    for f, r in both:
        print('   %-45s holes=%d nm=%d s2=%s' % (
            f, r.get('strict_holes'), r.get('strict_non_manifold'), r.get('stage2_ok')))
    print('strict watertight but pipeline NOT watertight: %d' % len(claim_gap))
    for f, r in claim_gap:
        print('   %-45s cat=%s s2=%s s2err=%s' % (
            f, r.get('category'), r.get('stage2_ok'), (r.get('stage2_error') or '')[:50]))

    # manifold3d cross-validation consistency (independent verdict vs the
    # defects.detect() strict check). 'n/a' meshes = manifold3d unavailable
    # in this environment (Linux py3.14 pymeshlab venv has no wheel).
    m3_res = [r for r in results.values() if r.get('manifold3d_watertight') is not None]
    m3_none = sum(1 for r in results.values() if r.get('manifold3d_watertight') is None)
    if m3_res:
        agree = sum(1 for r in m3_res if r.get('m3d_consistent'))
        disagree = [(f, r) for f, r in results.items()
                    if r.get('m3d_consistent') is False]
        print()
        print('=== manifold3d cross-validation (independent watertight check) ===')
        print('checked meshes        : %d (n/a when manifold3d absent: %d)' % (
            len(m3_res), m3_none))
        print('agreement with strict : %d (%.1f%%)' % (
            agree, 100.0 * agree / len(m3_res)))
        print('disagreements         : %d' % len(disagree))
        for f, r in disagree:
            print('   %-45s strict=%s m3d=%s (%s)' % (
                f, r.get('strict_watertight'), r.get('manifold3d_watertight'),
                r.get('manifold3d_status')))
    else:
        print()
        print('=== manifold3d cross-validation ===')
        print('manifold3d unavailable in this environment; column is n/a (%d meshes)' % m3_none)

    # Self-intersections before/after (input SI measured by the harness,
    # output SI measured on the final arrays). Separate signal from the
    # strict holes/nm watertight metric.
    si_in = [r.get('input_self_intersections') for r in results.values()
             if isinstance(r.get('input_self_intersections'), int)]
    si_out = [r.get('output_self_intersections') for r in results.values()
              if isinstance(r.get('output_self_intersections'), int)]
    if si_in:
        print()
        print('=== self-intersections (input -> output) ===')
        print('meshes with input SI    : %d' % sum(1 for s in si_in if s > 0))
        print('total input SI faces    : %d' % sum(si_in))
        print('total output SI faces   : %d' % sum(si_out))
        print('meshes with output SI   : %d' % sum(1 for s in si_out if s > 0))
        worst = sorted(((r.get('file'), r.get('input_self_intersections'),
                         r.get('output_self_intersections'))
                        for r in results.values()
                        if isinstance(r.get('input_self_intersections'), int)),
                       key=lambda x: -(x[1] or 0))[:10]
        print('worst input-SI meshes:')
        for f, si, so in worst:
            print('   %-45s in=%d out=%s' % (f, si, so))

    # fTetWild fallback tier summary (FAZ17) when the flag was enabled.
    ft_meshes = [r for r in results.values() if r.get('ftetwild_ran')]
    if ft_meshes:
        adopted = [r for r in ft_meshes if r.get('ftetwild_adopted')]
        pp = [r for r in adopted if r.get('ftetwild_manifold_postprocessed')]
        not_adopted = [r for r in ft_meshes if not r.get('ftetwild_adopted')]
        err = [r for r in ft_meshes if r.get('ftetwild_error')]
        timeouts = [r for r in err if r.get('ftetwild_error') == 'timeout']
        times = [r.get('ftetwild_time') for r in ft_meshes
                 if isinstance(r.get('ftetwild_time'), (int, float))]
        print()
        print('=== fTetWild fallback tier (--experimental-fallback-ftetwild) ===')
        print('meshes where it ran      : %d' % len(ft_meshes))
        print('adopted                  : %d' % len(adopted))
        print('  of which manifold3d post-processed : %d' % len(pp))
        print('adopt=False (kept stage-1) : %d' % len(not_adopted))
        print('errors                   : %d (of which %d timeout)' % (
            len(err), len(timeouts)))
        if timeouts:
            print('timeout meshes:')
            for r in sorted(timeouts, key=lambda x: x.get('file')):
                print('   %-45s' % r.get('file'))
        if times:
            import statistics
            print('ftetwild time (s): min=%.2f median=%.2f max=%.2f total=%.2f'
                  % (min(times), statistics.median(times), max(times), sum(times)))
        wt_gain = sum(1 for r in adopted if r.get('strict_watertight'))
        print('adopted meshes that end strictly watertight: %d' % wt_gain)
        if not_adopted:
            print('adopt=False meshes:')
            for r in sorted(not_adopted, key=lambda x: x.get('file')):
                print('   %-45s holes=%s nm=%s' % (
                    r.get('file'), r.get('strict_holes'), r.get('strict_non_manifold')))
    print('wrote %s' % out_path)


if __name__ == '__main__':
    main()