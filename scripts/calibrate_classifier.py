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
from repair import MECH_TUNE_GATE, ORG_TUNE_GATE  # noqa: E402

CLASSES = ('mechanical', 'organic')


def tuning_applied(pred, conf):
    """Mirror of the repair.py confidence gate: whether the tuned Stage 1
    thresholds would actually be used for this classification."""
    if pred == 'mechanical':
        return conf >= MECH_TUNE_GATE
    if pred == 'organic':
        return conf >= ORG_TUNE_GATE
    return False


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
            'tuning': tuning_applied(r['type'], r['confidence']),
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

    # --- tuning gate (repair.py MECH_TUNE_GATE / ORG_TUNE_GATE) -----------
    print()
    print('Tuning gate (MECH_TUNE_GATE=%.2f, ORG_TUNE_GATE=%.2f):' % (
        MECH_TUNE_GATE, ORG_TUNE_GATE))
    n_tuned = sum(1 for r in rows if r['tuning'])
    n_gated = sum(1 for r in rows if not r['tuning'])
    print('  tuned        : %d/%d' % (n_tuned, total))
    print('  below gate   : %d/%d (classification reported, defaults used)'
          % (n_gated, total))
    for r in rows:
        if not r['tuning'] and r['pred'] != 'unknown':
            print('    %-22s %-10s conf=%.3f -> defaults' % (
                r['name'], r['pred'], r['conf']))

    # --- per-mesh detail ---------------------------------------------------
    print()
    print('%-22s %-10s %-10s %-8s %-8s %-8s %s' %
          ('name', 'truth', 'pred', 'conf', 'near90', 'coplanar', 'tuned'))
    for r in rows:
        flag = '' if r['truth'] == r['pred'] else '   <--'
        tuned = 'Y' if r['tuning'] else 'N'
        print('%-22s %-10s %-10s %-8.3f %-8s %-8s %s%s' % (
            r['name'], r['truth'], r['pred'], r['conf'],
            r['near90'], r['coplanar'], tuned, flag))
    return 0


if __name__ == '__main__':
    sys.exit(main())