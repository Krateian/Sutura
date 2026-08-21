#!/usr/bin/env python3
"""Calibration harness for the mesh classifier.

Runs the current ``mesh_classifier.classify_mesh`` over the labeled synthetic
set from ``tests/make_classifier_set.py`` and reports:
  * per-class precision / recall (unknown counts as a miss)
  * the overall unknown rate
  * how confidence is distributed on correct vs wrong vs unknown predictions
    (a good confidence is high on correct, low on wrong)
  * a per-mesh detail table

Usage (needs the venv, trimesh is used to build the set):
    ~/.local/share/sutura/venv/bin/python scripts/calibrate_classifier.py

No files are written and the classifier itself is never modified -- this is a
read-only measurement tool for baseline and post-change comparison.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
TESTS = os.path.join(REPO, 'tests')
for p in (SUTURA, TESTS):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

from mesh_classifier import classify_mesh  # noqa: E402
from make_classifier_set import iter_meshes  # noqa: E402

CLASSES = ('mechanical', 'organic')


def _conf_summary(confs):
    if not confs:
        return 'n/a'
    a = np.array(confs)
    return 'mean=%.3f min=%.3f max=%.3f (n=%d)' % (a.mean(), a.min(), a.max(), len(a))


def main():
    rows = []
    for m in iter_meshes():
        r = classify_mesh(m['verts'], m['tris'])
        rows.append({
            'name': m['name'],
            'truth': m['label'],
            'pred': r['type'],
            'conf': r['confidence'],
            'near90': r['metrics']['near90'],
            'coplanar': r['metrics']['coplanar'],
        })

    total = len(rows)
    print('=== mesh classifier calibration (n=%d meshes) ===' % total)
    print()

    # --- per-class precision / recall -------------------------------------
    print('Per-class precision / recall (unknown predicted => miss):')
    for cls in CLASSES:
        tp = sum(1 for r in rows if r['truth'] == cls and r['pred'] == cls)
        fp = sum(1 for r in rows if r['truth'] != cls and r['pred'] == cls)
        fn = sum(1 for r in rows if r['truth'] == cls and r['pred'] != cls)
        prec = tp / (tp + fp) if tp + fp else float('nan')
        rec = tp / (tp + fn) if tp + fn else float('nan')
        print('  %-10s precision=%.3f recall=%.3f  (TP=%d FP=%d FN=%d)'
              % (cls, prec, rec, tp, fp, fn))

    correct = sum(1 for r in rows if r['truth'] == r['pred'])
    unknown = sum(1 for r in rows if r['pred'] == 'unknown')
    print('  accuracy=%.3f (correct %d/%d)   unknown-rate=%.3f (%d/%d)'
          % (correct / total, correct, total, unknown / total, unknown, total))

    # --- confidence separation --------------------------------------------
    print()
    print('Confidence distribution (higher on correct is better):')
    conf_correct = [r['conf'] for r in rows if r['truth'] == r['pred']]
    conf_wrong = [r['conf'] for r in rows
                  if r['truth'] != r['pred'] and r['pred'] != 'unknown']
    conf_unknown = [r['conf'] for r in rows if r['pred'] == 'unknown']
    print('  correct : %s' % _conf_summary(conf_correct))
    print('  wrong   : %s' % _conf_summary(conf_wrong))
    print('  unknown : %s' % _conf_summary(conf_unknown))

    # --- per-mesh detail ---------------------------------------------------
    print()
    print('%-22s %-10s %-10s %-8s %-8s %-8s' %
          ('name', 'truth', 'pred', 'conf', 'near90', 'coplanar'))
    for r in rows:
        flag = '' if r['truth'] == r['pred'] else '   <--'
        print('%-22s %-10s %-10s %-8.3f %-8s %-8s%s' % (
            r['name'], r['truth'], r['pred'], r['conf'],
            r['near90'], r['coplanar'], flag))
    return 0


if __name__ == '__main__':
    sys.exit(main())