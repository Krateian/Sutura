"""Repair-corpus benchmark with a STRICT watertight metric.

Runs the current repair pipeline (auto mode) over every STL in a corpus
directory and measures the FINAL output geometry (the exact arrays that are
saved to the `_fixed` file) with a strict defects.detect() closed-loop check:

    strict_watertight == (holes == 0 and non_manifold == 0)

The pipeline's own verdict (`repair_mesh_from_arrays` + stage 2) is recorded
alongside so a claim-vs-check discrepancy is visible, and input defects are
captured with the same detector for an input-vs-output comparison.

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


def load_input(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    return v, t


def run_one(src, tmpdir):
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

        rep, new_v, new_t = repair.repair_mesh_from_arrays(v, t, tmpdir, mode='auto')
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
    corpus = sys.argv[1] if len(sys.argv) > 1 else '/tmp/sutura_corpus_100'
    out_path = sys.argv[2] if len(sys.argv) > 2 else '/tmp/sutura_repair_benchmark.json'
    files = sorted(f for f in os.listdir(corpus) if f.lower().endswith('.stl'))
    print('corpus: %s (%d STL files)' % (corpus, len(files)), flush=True)

    results = {}
    failures = []
    for i, f in enumerate(files, 1):
        tmp = tempfile.mkdtemp(prefix='sutura-bench-')
        entry = err = None
        try:
            entry, err = run_one(os.path.join(corpus, f), tmp)
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
            print('[%3d/%d] %-48s cat=%-10s strict=%s (holes=%d nm=%d)' % (
                i, len(files), f[:48], str(entry.get('category')),
                entry['strict_watertight'], entry['strict_holes'],
                entry['strict_non_manifold']), flush=True)
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
    print('wrote %s' % out_path)


if __name__ == '__main__':
    main()